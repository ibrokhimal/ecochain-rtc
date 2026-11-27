//! Bounded, server-authoritative meeting workspace. Never performs I/O.
use crate::protocol::{ErrorCode, ProtocolError};
use serde_json::{json, Value};

const MAX_ACTION_BYTES: usize = 32 * 1024;
const MAX_SNAPSHOT_BYTES: usize = 10 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct Collaboration {
    pub state: Value,
    rates: std::collections::HashMap<String, (std::time::Instant, u32)>,
}

impl Default for Collaboration {
    fn default() -> Self {
        Self {
            rates: Default::default(),
            state: json!({
                "instance_id": uuid::Uuid::new_v4().to_string(), "revision": 0,
                "presentation_policy":"everyone", "presentation": null, "presentation_offer":null, "feedback":{}, "room_locked":false, "lobby_enabled":false, "drawing_policy": {"mode":"presenter", "participant_ids":[]},
                "objects":[], "presentations":[], "hands":[], "polls":[], "questions":[],
                "agenda":[], "notes":[], "decisions":[], "tasks":[]
            }),
        }
    }
}

impl Collaboration {
    pub fn apply(
        &mut self,
        author: &str,
        identity: &str,
        host: bool,
        action: Value,
    ) -> Result<Value, ProtocolError> {
        if action.to_string().len() > MAX_ACTION_BYTES {
            return Err(limit("action too large"));
        }
        self.rate(author, 100)?;
        // Validate and mutate a candidate; failure never partially commits an action.
        let mut next = self.state.clone();
        apply(&mut next, author, identity, host, &action)?;
        next["revision"] = json!(self.state["revision"].as_u64().unwrap_or(0) + 1);
        if next.to_string().len() > MAX_SNAPSHOT_BYTES {
            return Err(limit("meeting workspace full"));
        }
        self.state = next;
        Ok(self.state.clone())
    }

    pub fn allows_drawing(&self, author: &str, host: bool) -> bool {
        can_draw(&self.state, author, host)
    }

    pub fn prepare(
        &mut self,
        author: &str,
        identity: &str,
        host: bool,
        action: Value,
    ) -> Result<Self, ProtocolError> {
        let mut candidate = self.clone();
        candidate.apply(author, identity, host, action)?;
        self.rates = candidate.rates.clone();
        Ok(candidate)
    }

    fn rate(&mut self, author: &str, max: u32) -> Result<(), ProtocolError> {
        let entry = self
            .rates
            .entry(author.to_owned())
            .or_insert((std::time::Instant::now(), 0));
        if entry.0.elapsed() >= std::time::Duration::from_secs(1) {
            *entry = (std::time::Instant::now(), 0);
        }
        if entry.1 >= max {
            return Err(limit("too many collaboration updates"));
        }
        entry.1 += 1;
        Ok(())
    }

    pub fn reaction(&mut self, author: &str, action: &Value) -> Result<Value, ProtocolError> {
        self.rate(author, 5)?;
        let reaction = text(action, "reaction", 32)?;
        if !["thumbs_up", "clap", "heart"].contains(&reaction) {
            return Err(bad("invalid reaction"));
        }
        Ok(json!({"participant_id":author,"reaction":reaction}))
    }

    pub fn pointer(
        &mut self,
        author: &str,
        host: bool,
        action: &Value,
    ) -> Result<Value, ProtocolError> {
        require(can_draw(&self.state, author, host))?;
        require_presentation(&self.state, action)?;
        self.rate(author, 20)?;
        let point = &action["point"];
        if point.as_array().is_none_or(|p| {
            p.len() != 2
                || p.iter().any(|v| {
                    v.as_f64()
                        .is_none_or(|x| !x.is_finite() || !(0.0..=1.0).contains(&x))
                })
        }) {
            return Err(bad("invalid pointer"));
        }
        Ok(
            json!({"participant_id":author,"presentation_id":self.state["presentation"]["id"],"point":point,"laser":action["laser"]==true}),
        )
    }

    pub fn remove_participant(&mut self, id: &str) -> bool {
        self.rates.remove(id);
        let mut changed = self.state["feedback"]
            .as_object_mut()
            .unwrap()
            .remove(id)
            .is_some();
        if self.state["presentation_offer"]["participant_id"] == id {
            self.state["presentation_offer"] = Value::Null;
            changed = true;
        }
        if self.state["presentation"]["presenter_id"] == id {
            self.state["presentation"] = Value::Null;
            changed = true;
        }
        let hands = self.state["hands"].as_array_mut().unwrap();
        let before = hands.len();
        hands.retain(|v| v["participant_id"] != id);
        changed |= before != hands.len();
        if changed {
            self.state["revision"] = json!(self.state["revision"].as_u64().unwrap() + 1);
        }
        changed
    }
}

fn apply(
    s: &mut Value,
    author: &str,
    identity: &str,
    host: bool,
    a: &Value,
) -> Result<(), ProtocolError> {
    match text(a, "type", 64)? {
        "feedback.set" => {
            let feedback = text(a, "value", 16)?;
            if !["understood", "confused", "clear"].contains(&feedback) {
                return Err(bad("invalid feedback"));
            }
            if feedback == "clear" {
                s["feedback"].as_object_mut().unwrap().remove(author);
            } else {
                s["feedback"][author] = json!(feedback);
            }
        }
        "presentation.policy" => {
            require(host)?;
            let mode = text(a, "mode", 32)?;
            if !["everyone", "host_or_invited"].contains(&mode) {
                return Err(bad("invalid presentation policy"));
            }
            s["presentation_policy"] = json!(mode);
            if mode == "host_or_invited"
                && !s["presentation"].is_null()
                && s["presentation"]["presenter_host"] != true
                && s["presentation"]["invited"] != true
            {
                s["presentation"] = Value::Null;
            }
        }
        "presentation.offer" => {
            require(host || presenter(s, author))?;
            s["presentation_offer"] =
                json!({"participant_id":text(a,"participant_id",128)?,"from":author});
        }
        "presentation.decline" => {
            require(host || s["presentation_offer"]["participant_id"] == author)?;
            s["presentation_offer"] = Value::Null;
        }
        "room.lock" => {
            require(host)?;
            s["room_locked"] = json!(a["locked"]
                .as_bool()
                .ok_or_else(|| bad("locked must be boolean"))?);
        }
        "lobby.configure" => {
            require(host)?;
            s["lobby_enabled"] = json!(a["enabled"]
                .as_bool()
                .ok_or_else(|| bad("enabled must be boolean"))?);
        }
        "drawing.policy" => {
            require(host)?;
            let mode = text(a, "mode", 16)?;
            if !["presenter", "selected", "everyone"].contains(&mode) {
                return Err(bad("invalid drawing policy"));
            }
            let ids = a.get("participant_ids").cloned().unwrap_or(json!([]));
            if !ids.is_array()
                || ids.as_array().unwrap().len() > 150
                || ids
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|v| v.as_str().is_none_or(|s| s.len() > 128))
            {
                return Err(bad("invalid participants"));
            }
            s["drawing_policy"] = json!({"mode":mode,"participant_ids":ids});
        }
        "presentation.begin" | "board.open" => {
            let invited = s["presentation_offer"]["participant_id"] == author;
            require(host || s["presentation_policy"] != "host_or_invited" || invited)?;
            let kind = if a["type"] == "board.open" {
                "board"
            } else {
                "screen"
            };
            if !s["presentation"].is_null()
                && s["presentation"]["presenter_id"] != author
                && !host
                && s["presentation_offer"]["participant_id"] != author
            {
                return Err(forbidden());
            }
            if s["presentations"].as_array().unwrap().len() >= 1000 {
                return Err(limit("too many presentations"));
            }
            s["presentation"] = json!({"id":uuid::Uuid::new_v4().to_string(),"presenter_id":author,"kind":kind,"paused":false,"page":0,"invited":invited,"presenter_host":host});
            s["presentation_offer"] = Value::Null;
            let presentation = s["presentation"].clone();
            s["presentations"]
                .as_array_mut()
                .unwrap()
                .push(presentation);
        }
        "presentation.end" => {
            require(presenter(s, author) || host)?;
            s["presentation"] = Value::Null;
        }
        "presentation.pause" => {
            require(presenter(s, author) || host)?;
            if s["presentation"].is_null() {
                return Err(bad("no presentation"));
            }
            s["presentation"]["paused"] = json!(a["paused"]
                .as_bool()
                .ok_or_else(|| bad("paused must be boolean"))?);
        }
        "board.page" => {
            require(presenter(s, author) || host)?;
            if s["presentation"]["kind"] != "board" {
                return Err(bad("not a board"));
            }
            let page = a["page"]
                .as_u64()
                .filter(|p| *p < 100)
                .ok_or_else(|| bad("invalid board page"))?;
            s["presentation"]["page"] = json!(page);
        }
        "object.add" => {
            require(can_draw(s, author, host))?;
            require_presentation(s, a)?;
            let o = &a["object"];
            let id = text(o, "id", 128)?;
            if s["objects"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["id"] == id)
            {
                return Err(bad("object id already exists"));
            }
            let tool = text(o, "tool", 16)?;
            if ![
                "pen",
                "highlighter",
                "arrow",
                "rectangle",
                "ellipse",
                "text",
                "sticky",
                "image",
            ]
            .contains(&tool)
            {
                return Err(bad("unsupported drawing tool"));
            }
            let points = o["points"]
                .as_array()
                .filter(|p| !p.is_empty() && p.len() <= 2048)
                .ok_or_else(|| bad("invalid stroke points"))?;
            for point in points {
                let xy = point
                    .as_array()
                    .filter(|p| p.len() == 2)
                    .ok_or_else(|| bad("invalid point"))?;
                if xy.iter().any(|v| {
                    v.as_f64()
                        .is_none_or(|f| !f.is_finite() || !(0.0..=1.0).contains(&f))
                }) {
                    return Err(bad("point outside canvas"));
                }
            }
            let width = o["width"]
                .as_f64()
                .filter(|w| w.is_finite() && *w > 0.0 && *w <= 0.1)
                .ok_or_else(|| bad("invalid width"))?;
            let color = o["color"]
                .as_u64()
                .filter(|c| *c <= u32::MAX as u64)
                .ok_or_else(|| bad("invalid color"))?;
            if tool == "image"
                && s["objects"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|v| {
                        v["tool"] == "image"
                            && v["deleted"] != true
                            && v["presentation_id"] == s["presentation"]["id"]
                            && v["page"] == s["presentation"]["page"]
                    })
                    .count()
                    >= 32
            {
                return Err(limit("at most 32 images per page"));
            }
            let image_id = o.get("file_id").and_then(Value::as_str);
            if tool == "image" && image_id.is_none_or(|id| uuid::Uuid::parse_str(id).is_err()) {
                return Err(bad("invalid image reference"));
            }
            let body = o.get("text").and_then(Value::as_str).unwrap_or("");
            if body.len() > 4096 {
                return Err(bad("text too long"));
            }
            let item = json!({"id":id,"tool":tool,"color":color,"width":width,"points":points,"text":body,
                "file_id":image_id,"author_id":author,"revision":1,"deleted":false,"presentation_id":s["presentation"]["id"],"page":s["presentation"]["page"]});
            s["objects"].as_array_mut().unwrap().push(item);
        }
        "object.remove" | "object.restore" => {
            require(can_draw(s, author, host))?;
            let id = text(a, "id", 128)?;
            let can_clear = host || presenter(s, author);
            let presentation = s["presentation"]["id"].clone();
            let item = find(s, "objects", id)?;
            if item["presentation_id"] != presentation {
                return Err(bad("object belongs to a different presentation"));
            }
            require(item["author_id"] == author || can_clear)?;
            revision(item, a)?;
            item["deleted"] = json!(a["type"] == "object.remove");
            bump(item);
        }
        "object.clear" => {
            require(presenter(s, author) || host)?;
            require_presentation(s, a)?;
            let id = s["presentation"]["id"].clone();
            let page = s["presentation"]["page"].clone();
            for o in s["objects"].as_array_mut().unwrap() {
                if o["presentation_id"] == id && o["page"] == page && o["deleted"] != true {
                    o["deleted"] = json!(true);
                    bump(o);
                }
            }
        }
        "hand.set" => {
            let target = a
                .get("participant_id")
                .and_then(Value::as_str)
                .unwrap_or(author);
            require(target == author || host)?;
            let raised = a["raised"]
                .as_bool()
                .ok_or_else(|| bad("raised must be boolean"))?;
            require(!raised || target == author)?;
            let hands = s["hands"].as_array_mut().unwrap();
            if raised && !hands.iter().any(|v| v["participant_id"] == target) {
                hands.push(json!({"participant_id":target,"at":crate::auth::unix_now()}));
            } else if !raised {
                hands.retain(|v| v["participant_id"] != target);
            }
        }
        "poll.create" => {
            require(host)?;
            let id = text(a, "id", 128)?;
            let question = text(a, "question", 2000)?;
            let options = a["options"]
                .as_array()
                .filter(|o| (2..=10).contains(&o.len()))
                .ok_or_else(|| bad("need 2..10 options"))?;
            if options.iter().any(|v| {
                v.as_str()
                    .is_none_or(|v| v.trim().is_empty() || v.len() > 500)
            }) {
                return Err(bad("invalid poll option"));
            }
            insert(
                s,
                "polls",
                json!({"id":id,"question":question,"options":options,"votes":{},"closed":false,"author_id":author,"revision":1}),
            )?;
        }
        "poll.vote" => {
            let p = find(s, "polls", text(a, "id", 128)?)?;
            if p["closed"] == true {
                return Err(bad("poll closed"));
            }
            let option = a["option"]
                .as_u64()
                .filter(|v| *v < p["options"].as_array().unwrap().len() as u64)
                .ok_or_else(|| bad("invalid option"))?;
            p["votes"][identity] = json!(option);
            bump(p);
        }
        "poll.close" => {
            require(host)?;
            let p = find(s, "polls", text(a, "id", 128)?)?;
            p["closed"] = json!(true);
            bump(p);
        }
        "question.add" => {
            insert(
                s,
                "questions",
                json!({"id":text(a,"id",128)?,"text":text(a,"text",4000)?,"author_id":author,"votes":[],"answer":"","answered":false,"revision":1}),
            )?;
        }
        "question.vote" => {
            let q = find(s, "questions", text(a, "id", 128)?)?;
            let votes = q["votes"].as_array_mut().unwrap();
            if !votes.contains(&json!(identity)) {
                votes.push(json!(identity));
            }
            bump(q);
        }
        "question.answer" => {
            require(host)?;
            let q = find(s, "questions", text(a, "id", 128)?)?;
            revision(q, a)?;
            q["answer"] = json!(text(a, "text", 4000)?);
            q["answered"] = json!(true);
            bump(q);
        }
        "agenda.put" | "note.put" | "decision.put" | "task.put" => {
            let collection = match a["type"].as_str().unwrap() {
                "agenda.put" => "agenda",
                "note.put" => "notes",
                "decision.put" => "decisions",
                _ => "tasks",
            };
            if collection == "agenda" {
                require(host)?;
            }
            let id = text(a, "id", 128)?;
            let body = text(a, "text", 8000)?;
            let list = s[collection].as_array_mut().unwrap();
            let pos = list.iter().position(|v| v["id"] == id);
            let mut item = if let Some(pos) = pos {
                revision(&list[pos], a)?;
                list[pos].clone()
            } else {
                if a["expected_revision"].as_u64() != Some(0) {
                    return Err(conflict());
                }
                json!({"id":id,"author_id":author,"revision":0})
            };
            item["text"] = json!(body);
            if collection == "agenda" {
                item["duration_secs"] = json!(a["duration_secs"]
                    .as_u64()
                    .filter(|v| *v <= 86400)
                    .unwrap_or(300));
            }
            if collection == "tasks" {
                let status = a.get("status").and_then(Value::as_str).unwrap_or("open");
                if !["open", "in_progress", "done"].contains(&status) {
                    return Err(bad("invalid task status"));
                }
                item["status"] = json!(status);
                item["assignee"] = json!(a.get("assignee").and_then(Value::as_str).unwrap_or(""));
                item["due_date"] = json!(a.get("due_date").and_then(Value::as_str).unwrap_or(""));
                if !valid_date(item["due_date"].as_str().unwrap()) {
                    return Err(bad("invalid due date"));
                }
                if item["assignee"].as_str().unwrap().len() > 128
                    || item["due_date"].as_str().unwrap().len() > 32
                {
                    return Err(bad("invalid task fields"));
                }
            }
            bump(&mut item);
            if let Some(pos) = pos {
                list[pos] = item;
            } else {
                if list.len() >= 1000 {
                    return Err(limit("too many workspace items"));
                }
                list.push(item);
            }
        }
        "agenda.start" => {
            require(host)?;
            let id = text(a, "id", 128)?;
            find(s, "agenda", id)?;
            s["active_agenda"] = json!({"id":id,"started_at":crate::auth::unix_now()});
        }
        _ => return Err(bad("unknown meeting action")),
    }
    Ok(())
}
fn can_draw(s: &Value, author: &str, host: bool) -> bool {
    !s["presentation"].is_null()
        && s["presentation"]["paused"] != true
        && (host
            || presenter(s, author)
            || s["drawing_policy"]["mode"] == "everyone"
            || (s["drawing_policy"]["mode"] == "selected"
                && s["drawing_policy"]["participant_ids"]
                    .as_array()
                    .is_some_and(|ids| ids.contains(&json!(author)))))
}
fn presenter(s: &Value, author: &str) -> bool {
    s["presentation"]["presenter_id"] == author
}
fn require_presentation(s: &Value, a: &Value) -> Result<(), ProtocolError> {
    if s["presentation"].is_null() || a["presentation_id"] != s["presentation"]["id"] {
        Err(conflict())
    } else {
        Ok(())
    }
}
fn text<'a>(v: &'a Value, key: &str, max: usize) -> Result<&'a str, ProtocolError> {
    v[key]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= max)
        .ok_or_else(|| bad(format!("invalid {key}")))
}
fn find<'a>(s: &'a mut Value, key: &str, id: &str) -> Result<&'a mut Value, ProtocolError> {
    s[key]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|v| v["id"] == id)
        .ok_or_else(|| bad("unknown item"))
}
fn insert(s: &mut Value, key: &str, item: Value) -> Result<(), ProtocolError> {
    let list = s[key].as_array_mut().unwrap();
    if list.len() >= 1000 {
        return Err(limit("too many workspace items"));
    }
    if list.iter().any(|v| v["id"] == item["id"]) {
        return Err(bad("id already exists"));
    }
    list.push(item);
    Ok(())
}
fn revision(item: &Value, a: &Value) -> Result<(), ProtocolError> {
    if item["revision"].as_u64() != a["expected_revision"].as_u64() {
        Err(conflict())
    } else {
        Ok(())
    }
}
fn bump(item: &mut Value) {
    item["revision"] = json!(item["revision"].as_u64().unwrap_or(0) + 1);
}
fn require(ok: bool) -> Result<(), ProtocolError> {
    if ok {
        Ok(())
    } else {
        Err(forbidden())
    }
}
fn forbidden() -> ProtocolError {
    ProtocolError::new(ErrorCode::Forbidden, "meeting action is not permitted")
}
fn conflict() -> ProtocolError {
    ProtocolError::new(
        ErrorCode::InvalidState,
        "workspace changed; refresh before editing",
    )
}
fn bad(message: impl Into<String>) -> ProtocolError {
    ProtocolError::new(ErrorCode::BadRequest, message)
}
fn limit(message: &str) -> ProtocolError {
    ProtocolError::new(ErrorCode::LimitExceeded, message)
}

fn valid_date(date: &str) -> bool {
    if date.is_empty() {
        return true;
    }
    let b = date.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || b.iter()
            .enumerate()
            .any(|(i, c)| i != 4 && i != 7 && !c.is_ascii_digit())
    {
        return false;
    }
    let year: u32 = date[..4].parse().unwrap();
    let month: usize = date[5..7].parse().unwrap();
    let day: u32 = date[8..].parse().unwrap();
    if year == 0 || !(1..=12).contains(&month) {
        return false;
    }
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    day > 0 && day <= days[month - 1]
}

/// Mutations carry only changed items, keeping fanout and dedup caches bounded.
pub(super) fn delta(previous: &Value, next: &Value) -> Value {
    let mut set = serde_json::Map::new();
    let mut collections = serde_json::Map::new();
    for (key, value) in next.as_object().unwrap() {
        if key == "revision" || key == "instance_id" || previous.get(key) == Some(value) {
            continue;
        }
        if [
            "objects",
            "hands",
            "polls",
            "questions",
            "agenda",
            "notes",
            "decisions",
            "tasks",
        ]
        .contains(&key.as_str())
        {
            let old = previous[key].as_array().unwrap();
            let new = value.as_array().unwrap();
            let id_key = if key == "hands" {
                "participant_id"
            } else {
                "id"
            };
            let old_index: std::collections::HashMap<_, _> = old
                .iter()
                .map(|v| (v[id_key].as_str().unwrap(), v))
                .collect();
            let new_ids: std::collections::HashSet<_> =
                new.iter().map(|v| v[id_key].as_str().unwrap()).collect();
            let upsert: Vec<_> = new
                .iter()
                .filter_map(|item| {
                    let id = item[id_key].as_str().unwrap();
                    match old_index.get(id) {
                        Some(old) if **old == *item => None,
                        Some(old) => {
                            let mut fields = serde_json::Map::new();
                            fields.insert(id_key.to_owned(), item[id_key].clone());
                            for (key, value) in item.as_object().unwrap() {
                                if old.get(key) != Some(value) {
                                    fields.insert(key.clone(), value.clone());
                                }
                            }
                            Some(Value::Object(fields))
                        }
                        None => Some(item.clone()),
                    }
                })
                .collect();
            let removed: Vec<_> = old
                .iter()
                .filter(|v| !new_ids.contains(v[id_key].as_str().unwrap()))
                .map(|v| v[id_key].clone())
                .collect();
            collections.insert(key.clone(), json!({"upsert":upsert,"remove_ids":removed}));
        } else {
            set.insert(key.clone(), value.clone());
        }
    }
    json!({"instance_id":next["instance_id"],"revision":next["revision"],"patch":{"set":set,"collections":collections}})
}
