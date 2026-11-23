# 100+ kishilik xonalar — amalga oshirish rejasi

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bitta SFU xonasi 100+ ishtirokchini (1–2 gapiruvchi, qolganlar tinglovchi) past kechikish va yuqori sifat bilan ko‘tarsin.

**Architecture:** Xona aktori modeli saqlanadi. Har paketdagi O(N) qidiruvlar indekslar bilan almashtiriladi, audio faqat eng baland N trek uchun yoziladi (A yo‘l), keyframe so‘rovlari birlashtiriladi, TURN uchun vaqtinchalik ma’lumotlar beriladi. Har qadam `loadtest` bilan o‘lchanadi.

**Tech Stack:** Rust, tokio, `str0m` =0.25.0, axum; testlar `cargo test` (integratsiya: `server/tests/*.rs`, headless `bot.rs`).

**Spec:** `docs/superpowers/specs/2026-11-20-100-participant-rooms-design.md`

Barcha buyruqlar `ecochain-rtc/server` papkasidan ishga tushiriladi. Ish alohida branch’da (`feat/100-participant-rooms`).

## Global Constraints

* `str0m` versiyasi `=0.25.0`, frame mode — o‘zgarmaydi.
* Flutter SDK (`packages/`) o‘zgarmaydi — na ommaviy API, na ichki qism.
* Protokol orqaga mos: mavjud xabar/maydonlar o‘zgarmaydi; yangilari ixtiyoriy.
* `rooms.max_forwarded_audio`: standart **3**, ruxsat **1..=8**.
* `rooms.max_participants_limit`: standart **150**; `rooms.default_max_participants`: **25** (o‘zgarmaydi).
* Audio tanlovchi: ostona = mavjud `SPEAKING_LEVEL` (chiziqli **0.03**, ≈ −30 dBov); almashtirish farqi **6 dB**; ushlab turish **500 ms**; ko‘tarilish vaqt doimiysi **20 ms**, tushish **300 ms**.
* `speakers.changed` **250 ms**’dan tez yuborilmaydi, formati o‘zgarmaydi.
* Keyframe throttle: oddiy **500 ms**, yangi obuna (urgent) **150 ms**, (trek, rid) bo‘yicha.
* Yangi bog‘liqlik faqat `sha1 = "0.10"` (TURN HMAC-SHA1, mavjud `hmac 0.12` bilan).
* Mavjud barcha testlar (`cargo test`) har vazifadan keyin o‘tishi shart.

## Review Focus

1. Tanlangan (gapirayotgan) ishtirokchi chiqib ketsa yoki resume qilsa — slot darhol bo‘shashi, keyingi gapiruvchi kesilmasligi kerak → Task 5 testi `leaving_speaker_frees_audio_slot`.
2. Klient manzili o‘zgarsa (Wi‑Fi → mobil, NAT rebinding) — eski marshrut keshi xato peer’ga olib bormasligi, fallback topishi kerak → Task 2 da mavjud `resilience.rs` switch-network testlari majburiy ishga tushiriladi + `stale_route_falls_back_to_scan`.
3. Fon shovqini ostonadan baland tinglovchilar faol gapiruvchini siqib chiqarmasligi kerak → Task 4 `noise_below_margin_cannot_evict`.
4. `audio_level` kengaytmasisiz eski klient ovozi yo‘qolmasligi kerak → Task 4 `missing_level_is_always_forwarded`.
5. 98 kishi bir vaqtda bitta videoga obuna bo‘lsa, publisher keyframe’larga ko‘milmasligi va hamma video olishi kerak → Task 6 `burst_of_subscribers_gets_video_with_few_keyframes`.

---

### Task 1: Yuklama testi ssenariylari va xona band-vaqti metrikasi

**Files:**
- Modify: `src/bot.rs` (`BotOptions`, `BotStats`, `Control`, `Runner::drive`, `Runner::send_media`, `handle_event`, `subscribe_videos`)
- Modify: `src/bin/loadtest.rs`
- Modify: `src/metrics.rs`, `src/room/mod.rs` (`Room::run`)
- Modify: `tests/common/mod.rs` (`TestServer::bot` yangi maydonlar standartlari)
- Test: `tests/media.rs`

**Interfaces:**
- Produces (`bot.rs`):
  * `BotOptions` yangi maydonlari: `audio_level: i8` (standart −25), `audio_muted: bool` (audio m-line muzokara qilinadi, paket yuborilmaydi, server SDK’dagi kabi mute xabarini oladi), `high_video: usize` (birinchi `high_video` ta obuna `"high"`, qolganlari `"low"`; `usize::MAX` = hozirgi xatti-harakat).
  * `BotStats` yangi maydonlari: `audio_frames_by_participant: HashMap<String, u64>`, `video_frames_by_participant: HashMap<String, u64>` (mid → participant `subscriber.offer.tracks` mappinglaridan).
  * `Bot::subscribe_video(&self, track_id: String, quality: &'static str)`, `Bot::unsubscribe_video(&self, track_id: String)`, `Bot::set_audio_muted(&self, muted: bool)`, `Bot::enable_audio(&self)` (audio trekni qo‘shib publisher’ni qayta muzokara qiladi — “birinchi yoqish”).
  * `BotStats::remote_tracks: Vec<(String /*participant*/, String /*track*/, String /*kind*/)>`.
  * `BotStats::keyframe_requests: u64` — publisher peer’da olingan `KeyframeRequest` hodisalari soni (Task 6 testi uchun).
- Produces (`metrics.rs`): `Metrics::room_busy_micros_total: AtomicU64`, `/metrics`da `ecochain_rtc_room_busy_seconds_total`.

- [ ] **Step 1: Failing test — per-participant hisoblagichlar**

`tests/media.rs`:
```rust
#[tokio::test]
async fn bot_counts_frames_per_participant() {
    // alice audio+video, bob faqat qabul qiladi
    // kutish: bob.stats().audio_frames_by_participant[alice_id] > 50
    //         bob.stats().video_frames_by_participant[alice_id] > 25
}
```
- [ ] **Step 2:** `cargo test --test media bot_counts_frames_per_participant` → FAIL (maydon yo‘q).
- [ ] **Step 3:** `BotOptions`/`BotStats`/`Control` maydonlari va usullarini qo‘shish; `drive` MediaData’ni mid → participant xaritasi bo‘yicha hisoblaydi; `send_media` `options.audio_level` va `audio_muted`ni hurmat qiladi; `subscribe_videos` `high_video` bo‘yicha quality tanlaydi. Mute xabari — SDK `_sendMute` yuboradigan so‘rovning o‘zi (`docs/PROTOCOL.md`).
- [ ] **Step 4:** test PASS, keyin `cargo test` — hammasi PASS.
- [ ] **Step 5: Xona band vaqti.** `Room::run` sikli ichida `handle_*` + `drive` atrofida `Instant` bilan vaqt o‘lchanadi va `room_busy_micros_total`ga qo‘shiladi. Test: `tests/media.rs::room_busy_metric_grows` — 2 bot 2 s ishlagach `server.metric("ecochain_rtc_room_busy_seconds_total")` > 0 (metrika butun soniyada bo‘lsa, mikro qiymat bilan ham chiqarilsin, test shunga mos).
- [ ] **Step 6: loadtest flaglari**

`loadtest.rs` yangi argumentlari: `--publishers N` (standart = `participants`, hozirgi xatti-harakat), `--cameras M` (faqat kamera, standart 0), `--muted-mics K` (audio muted, standart 0), `--high-video H` (standart `usize::MAX`), `--switch-every-secs S` (0 = o‘chiq), `--unmute-probe` (bitta tinglovchi `enable_audio` qiladi), `--metrics-url URL` (+ `--admin-key`).
Botlar tartibi: avval `publishers` (audio −25 dBov + 3 qatlamli video), keyin `cameras`, keyin `muted-mics`, qolganlar media’siz.
Chiqishga qo‘shiladigan qatorlar:
```text
switch to first frame  p50=…ms p95=…ms (n=…)
first unmute heard     p50=…ms p95=…ms
room busy              …% of one core
```
Kutilgan audio = `min(publishers, max_forwarded_audio)` (Task 5 dan keyin) — hozircha `publishers` (o‘zi bundan tashqari); `--expect-audio` flagi bilan qo‘lda berish mumkin.
- [ ] **Step 7: Asosiy chiziq.** Release build bilan o‘lchash va natijani yozib qo‘yish (TESTING.md ga Task 8 da kiritiladi):
```bash
cargo build --release
./target/release/loadtest --participants 100 --publishers 2 --cameras 10 --muted-mics 30 --max-video 4 --high-video 1 --switch-every-secs 5 --unmute-probe --seconds 30 --metrics-url http://127.0.0.1:7880/metrics
```
Kutish: dastur yakunlanadi va barcha qatorlarni chiqaradi (raqamlar hali mezonga yetmasligi mumkin). Shuningdek `--participants 25` mavjud ssenariylari.
- [ ] **Step 8: Commit** — `git commit -m "loadtest: speaker/listener scenarios, switch and unmute probes, room busy metric"`

---

### Task 2: Datagram marshrut keshi (`handle_datagram`)

**Files:**
- Modify: `src/room/mod.rs` (`Room` maydoni), `src/room/media.rs` (`handle_datagram`, `fail_peer`, `close_media`, `reset_subscriber`, `publisher_offer` reset yo‘li)
- Test: `tests/resilience.rs`, `tests/media.rs`

**Interfaces:**
- Produces: `Room::routes: HashMap<SocketAddr, (String /*participant id*/, PeerKind)>`; `Room::forget_routes(&mut self, participant_id: &str, kind: Option<PeerKind>)`.

- [ ] **Step 1: Failing/guard testlar**

`tests/media.rs`:
```rust
#[tokio::test]
async fn media_keeps_flowing_after_another_participant_leaves() {
    // a, b, c qo‘shiladi; b chiqadi; a va c bir-biridan audio/video olishda davom etadi
    // (2 s ichida har birida audio_frames_by_participant[other] o‘sgan)
}
```
`tests/resilience.rs`: `stale_route_falls_back_to_scan` — bot `switch_network()` qiladi; 5 s ichida media qaytadi (mavjud switch-network testi bilan bir xil yo‘l, lekin kesh yoqilgan holatda).
- [ ] **Step 2:** testlar hozir PASS bo‘lishi kerak (himoya testlari) — `cargo test --test media --test resilience`.
- [ ] **Step 3:** `handle_datagram`: avval `routes.get(&datagram.source)` → `index_of(id)` → peer `accepts` bo‘lsa ishlov; aks holda hozirgi to‘liq aylanish, topilgan peer keshga yoziladi (eski yozuv ustidan). Peer yopiladigan/almashtiriladigan barcha joylarda `forget_routes`.
- [ ] **Step 4:** `cargo test` — hammasi PASS.
- [ ] **Step 5:** Task 1 Step 7 buyrug‘i — `room busy` asosiy chiziqqa nisbatan qayd etiladi.
- [ ] **Step 6: Commit** — `git commit -m "room: cache datagram routes by source address"`

---

### Task 3: Trek → subscriber’lar indeksi (`forward`)

**Files:**
- Modify: `src/room/mod.rs` (`Room` maydonlari), `src/room/media.rs` (`forward`, `reconcile`, `subscriber_answer`, `fail_peer`, `close_media`)
- Test: `tests/media.rs`

**Interfaces:**
- Produces: `Room::forwarding: HashMap<String /*track id*/, Vec<String /*subscriber participant id*/>>`, `Room::forwarding_dirty: bool`, `Room::rebuild_forwarding(&mut self)` — faqat `OutState::Open` yozuvlaridan quriladi.

- [ ] **Step 1: Guard test** `tests/media.rs::subscriber_gets_track_only_after_answer_and_stops_after_unsubscribe` — bob alice videosiga obuna bo‘ladi → kadr keladi; `unsubscribe_video` → 1 s dan keyin `video_frames_by_participant[alice]` o‘smaydi.
- [ ] **Step 2:** `cargo test --test media` — hozirgi kodda PASS.
- [ ] **Step 3:** OutState o‘zgaradigan har joyda `forwarding_dirty = true`; `forward` boshida dirty bo‘lsa `rebuild_forwarding`; keyin faqat `forwarding[track_id]` dagi subscriber’lar aylaniladi. `wanted_keyframe` takrorlari `forward` ichida bitta (rid bo‘yicha) yozuvga birlashtiriladi.
- [ ] **Step 4:** `cargo test` — PASS.
- [ ] **Step 5:** loadtest o‘lchovi (Task 1 Step 7), natija qayd etiladi.
- [ ] **Step 6: Commit** — `git commit -m "room: index subscribers per track for forwarding"`

---

### Task 4: Audio tanlovchi (sof modul)

**Files:**
- Create: `src/room/audio.rs`
- Modify: `src/room/mod.rs` (`mod audio;`)

**Interfaces:**
- Produces:
```rust
pub struct AudioSelector { /* ... */ }
impl AudioSelector {
    pub fn new(max: usize) -> Self;
    /// Paket uchun: uzatilsinmi. level_dbov — RFC 6464 (0 eng baland, -127 jimlik), None = kengaytma yo‘q.
    pub fn observe(&mut self, track_id: &str, level_dbov: Option<i8>, muted: bool, now: Instant) -> bool;
    pub fn remove(&mut self, track_id: &str);
    /// Tanlangan treklar, eng balandi birinchi.
    pub fn selected(&self) -> Vec<String>;
}
```
Daraja chiziqli amplitudada eksponensial silliqlanadi (ko‘tarilish 20 ms, tushish 300 ms, `dt` paketlar orasidagi vaqtdan); taqqoslash va 6 dB farq dB’da.

- [ ] **Step 1: Failing unit testlar** (`audio.rs` ichida `#[cfg(test)]`, `Instant` bazaviy `t0` + `Duration`):
```rust
#[test] fn admits_immediately_when_slot_free()      // observe("a", Some(-25), false, t0) == true
#[test] fn silence_is_not_forwarded()               // observe("b", Some(-127), false, t0) == false
#[test] fn muted_is_never_forwarded()               // observe("a", Some(-10), true, t0) == false
#[test] fn missing_level_is_always_forwarded()      // observe("x", None, false, t0) == true; "x" selected()da yo‘q
#[test] fn noise_below_margin_cannot_evict()        // a,b,c −25 da 200 ms; d −22 da 500 ms → d false, selected = {a,b,c}
#[test] fn louder_newcomer_evicts_quietest()         // a,b −25, c −35; d −15 da 100 ms → d true, c selected()dan chiqqan
#[test] fn holds_through_short_pause()              // a −25 da 200 ms, keyin −127: +400 ms da true, +700 ms da false
#[test] fn remove_frees_slot()                      // a,b,c to‘la; remove("a"); d −30 → true
#[test] fn selected_is_loudest_first()
```
- [ ] **Step 2:** `cargo test --lib room::audio` → FAIL (modul yo‘q).
- [ ] **Step 3:** `AudioSelector`ni Global Constraints qiymatlari bilan amalga oshirish (konstantalar modul boshida, izoh bilan).
- [ ] **Step 4:** `cargo test --lib room::audio` → PASS.
- [ ] **Step 5: Commit** — `git commit -m "room: loudest-N audio selector"`

---

### Task 5: Tanlovchini uzatishga va `speakers.changed`ga ulash, config

**Files:**
- Modify: `src/config.rs` (`RoomsConfig::max_forwarded_audio`, fayl strukturasi, validatsiya 1..=8, standart 3; `max_participants_limit` standarti 150), `config/development.toml`, `config/production.example.toml` (izohli namuna)
- Modify: `src/room/mod.rs` (`Room::audio: AudioSelector`, `speakers`, `last_speaker_check` → `last_speakers_sent`), `src/room/participant.rs` (`level_sum`, `level_count` olib tashlanadi)
- Modify: `src/room/media.rs` (`forward`, `unpublish`, `update_speakers`, `media_housekeeping`)
- Test: `tests/media.rs`, `src/config.rs` testlari

**Interfaces:**
- Consumes: Task 4 `AudioSelector`, Task 1 `BotOptions::audio_level`, `audio_frames_by_participant`.
- Produces: `RoomsConfig::max_forwarded_audio: usize`.

- [ ] **Step 1: Failing testlar**

`tests/media.rs`:
```rust
#[tokio::test]
async fn only_loudest_three_audio_tracks_are_forwarded() {
    // 4 gapiruvchi: audio_level -10, -15, -20, -45 (video yo‘q); listener media’siz
    // 3 s dan keyin listener: dastlabki uchtasi > 100 kadr, -45 dagisi < 10 kadr
}
#[tokio::test]
async fn leaving_speaker_frees_audio_slot() {
    // 4 gapiruvchi -10,-15,-20,-25; -10 chiqadi → 2 s ichida -25 dagisi listenerga keladi
}
#[tokio::test]
async fn muted_audio_reaches_nobody() {
    // speaker audio_muted=true → listener 2 s da 0 audio kadr
}
```
`src/config.rs`: `max_forwarded_audio_defaults_to_three`, `max_forwarded_audio_out_of_range_is_rejected` (0 va 9), mavjud `default_max_participants == 25` testiga qo‘shib `max_participants_limit == 150`.
- [ ] **Step 2:** `cargo test` → yangi testlar FAIL.
- [ ] **Step 3:** `forward` audio uchun: `self.audio.observe(track_id, level, track.muted, now)` false bo‘lsa — subscriber’lar aylanishidan oldin qaytadi. `unpublish` (va trek o‘chadigan har joy) `audio.remove`. `update_speakers`: `audio.selected()` trek id → publisher id; ro‘yxat o‘zgarsa va oxirgi yuborishdan ≥ 250 ms o‘tgan bo‘lsa `speakers.changed` (format o‘zgarmaydi, `level` silliqlangan chiziqli qiymat); `media_housekeeping` va `forward`dan chaqiriladi; 250 ms tugagach yuborilmagan o‘zgarish `next_timeout` orqali yuboriladi. Join javobidagi `"speakers"` shu ro‘yxatdan.
- [ ] **Step 4:** `cargo test` — hammasi PASS (shu jumladan speaker hodisasini tekshiruvchi mavjud testlar).
- [ ] **Step 5:** loadtest (`--expect-audio` endi kerak emas: kutilgan audio `min(publishers, 3)`), natija qayd etiladi.
- [ ] **Step 6: Commit** — `git commit -m "room: forward only the loudest N audio tracks, faster speaker events"`

---

### Task 6: Keyframe so‘rovlarini birlashtirish

**Files:**
- Create: `src/room/keyframe.rs`
- Modify: `src/room/participant.rs` (`PublishedTrack::last_keyframe_request` → `keyframes: HashMap<Option<Rid>, KeyframeGate>`), `src/room/media.rs` (`request_keyframe`, `subscriber_answer`, `allocate_layers`, `forward`, `next_timeout`), `src/room/mod.rs` (run siklida `flush_keyframes`)
- Test: `src/room/keyframe.rs` unit, `tests/media.rs`

**Interfaces:**
- Produces:
```rust
pub struct KeyframeGate { /* last: Option<Instant>, due: Option<Instant> */ }
impl KeyframeGate {
    /// true = hozir yuborilsin (va yuborilgan deb belgilanadi); false = due rejalashtirildi.
    pub fn request(&mut self, now: Instant, urgent: bool) -> bool;
    pub fn due(&self) -> Option<Instant>;
    /// due o‘tgan bo‘lsa true qaytaradi va yuborilgan deb belgilaydi.
    pub fn take_due(&mut self, now: Instant) -> bool;
}
```
- `Room::request_keyframe(&mut self, publisher_id: &str, track_id: &str, rid: Option<Rid>, kind: KeyframeRequestKind, urgent: bool)`; `Room::flush_keyframes(&mut self, now: Instant)`.
- urgent = `subscriber_answer` dagi yangi ochilgan video va `allocate_layers` dagi qatlam almashuvi; qolganlari (PLI forwarding, `!contiguous`) oddiy.

- [ ] **Step 1: Failing unit testlar**
```rust
#[test] fn first_request_is_sent()                       // request(t0,false) == true
#[test] fn request_within_throttle_is_deferred()          // t0 yuborilgan; request(t0+100ms,false)==false; due()==Some(t0+500ms)
#[test] fn urgent_uses_short_throttle()                   // t0 yuborilgan; request(t0+100ms,true)==false; due()==Some(t0+150ms)
#[test] fn urgent_after_short_throttle_is_sent_now()      // request(t0+160ms,true)==true
#[test] fn deferred_requests_coalesce()                    // 50 ta request throttle ichida → take_due(due) bir marta true, keyin false
```
- [ ] **Step 2:** `cargo test --lib room::keyframe` → FAIL.
- [ ] **Step 3:** `KeyframeGate`ni (500 ms / 150 ms) amalga oshirish; `request_keyframe` gate orqali; `flush_keyframes` barcha treklarning `take_due`larini yuboradi; `next_timeout` eng yaqin `due`ni hisobga oladi.
- [ ] **Step 4: Integratsiya testlari** `tests/media.rs`:
```rust
#[tokio::test]
async fn switching_to_another_video_shows_frame_quickly() {
    // alice va carol kamera (simulcast); bob alice’ga obuna, kadr oladi;
    // bob.subscribe_video(carol_track) → carol’dan birinchi kadr 1 s ichida (loopback)
}
#[tokio::test]
async fn burst_of_subscribers_gets_video_with_few_keyframes() {
    // 1 publisher (simulcast), 20 tinglovchi bir vaqtda obuna → hammasi 3 s ichida video oladi;
    // publisher olgan keyframe so‘rovlari (bot hisoblagichi `keyframe_requests`, Task 1 BotStats ga qo‘shiladi) ≤ 3 s / 150 ms * qatlamlar soni
}
```
- [ ] **Step 5:** `cargo test` — hammasi PASS.
- [ ] **Step 6:** loadtest `switch to first frame` p95 qayd etiladi.
- [ ] **Step 7: Commit** — `git commit -m "room: coalesce keyframe requests per track and layer, fast path for new subscriptions"`

---

### Task 7: TURN — vaqtinchalik ma’lumotlar va coturn hujjati

**Files:**
- Modify: `Cargo.toml` (`sha1 = "0.10"`)
- Create: `src/turn.rs` (`lib.rs`da `pub mod turn;`)
- Modify: `src/config.rs` (`FileMediaConfig::turn: Option<TurnConfig>`, `MediaConfig::turn`), `src/room/mod.rs` (join javobidagi `ice_servers`)
- Create: `deploy/coturn/turnserver.conf.example`
- Modify: `config/production.example.toml`, `docs/ARCHITECTURE.md` (Cheklovlar → TURN bo‘limi)
- Test: `src/turn.rs` unit, `tests/signaling.rs`

**Interfaces:**
- Produces:
```rust
pub struct TurnConfig { pub urls: Vec<String>, pub secret: Secret, pub ttl_secs: u64 } // ttl standart 3600
pub fn credentials(config: &TurnConfig, identity: &str, now_unix: u64) -> IceServer;
// username = format!("{}:{}", now_unix + ttl_secs, identity)
// credential = base64(HMAC-SHA1(secret, username))  (coturn use-auth-secret / TURN REST API)
```
`secret` mavjud `Secret` turi va inline-secret qoidalari bilan (production’da fayl/env orqali).

- [ ] **Step 1: Failing testlar**
  * `turn::tests::credential_matches_coturn_formula` — ma’lum secret `"s3cret"`, identity `"alice"`, `now=1_700_000_000`, ttl 3600 → username `"1700003600:alice"`, credential = test ichida `hmac`/`sha1` bilan alohida hisoblangan qiymatga teng.
  * `tests/signaling.rs::join_response_contains_turn_credentials` — `turn` sozlangan server; join javobida `ice_servers` ichida urls mos, username `":alice"` bilan tugaydi, credential bo‘sh emas; statik `ice_servers` ham saqlangan.
- [ ] **Step 2:** `cargo test` → FAIL.
- [ ] **Step 3:** `turn.rs`, config o‘qish/validatsiya (urls `turn:`/`turns:` bilan boshlanishi, ttl ≥ 60), join javobida statik ro‘yxat + `credentials(...)`.
- [ ] **Step 4:** `cargo test` — PASS.
- [ ] **Step 5:** `deploy/coturn/turnserver.conf.example` (`use-auth-secret`, `static-auth-secret`, `listening-port=3478`, `tls-listening-port=443`, `cert`/`pkey`, `no-multicast-peers`, `denied-peer-ip` xususiy diapazonlar, faqat SFU IP’siga `allowed-peer-ip`), ARCHITECTURE.md’ga o‘rnatish bo‘limi.
- [ ] **Step 6: Commit** — `git commit -m "turn: ephemeral TURN REST credentials and coturn example"`

---

### Task 8: Yakuniy o‘lchov va hujjatlar

**Files:**
- Modify: `docs/TESTING.md` (yangi bo‘lim: 100 kishilik ssenariy, har vazifadan keyingi raqamlar, sharoit), `docs/ARCHITECTURE.md` (Kengaytirish, Qurilmada qabul qilinadigan video soni, audio tanlovchi), `docs/PROTOCOL.md` (faqat yangi ixtiyoriy maydon qo‘shilgan bo‘lsa)

- [ ] **Step 1:** 16 yadroli Linux serverda (generator boshqa mashinada, LAN) Task 1 Step 7 buyrug‘i va `--participants 25` mavjud ssenariylari.
- [ ] **Step 2:** Spec mezonlari jadvali bilan solishtirish: audio ≥ 99%, video o‘rtacha ≥ 98% / min ≥ 95%, room busy ≤ 70%, switch p95 ≤ 500 ms, 25 kishida regressiya yo‘q; birinchi unmute kechikishi faqat yoziladi.
- [ ] **Step 3:** Bajarilmagan mezon bo‘lsa — TESTING.md’da ochiq yoziladi va spec bo‘yicha keyingi qadam (6-qism egress yoki video slotlar) taklif sifatida qayd etiladi; bu reja doirasida amalga oshirilmaydi.
- [ ] **Step 4:** Qo‘lda: Meet ilovasi (2 haqiqiy qurilma) + 50 bot; gapiruvchi almashganda ovoz uzilishi yo‘qligi va boshqa videoga o‘tish tezligi tekshiriladi, natija TESTING.md’ga.
- [ ] **Step 5: Commit** — `git commit -m "docs: 100 participant room results and architecture notes"`
