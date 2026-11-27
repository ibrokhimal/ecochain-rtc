# Ecochain RTC signaling protokoli (v1)

Kanal: WebSocket, `GET /v1/rtc`. Production’da faqat `wss://` (TLS). Har bir text frame — bitta JSON obyekt. Binary frame’lar e’tiborsiz qoldiriladi. Frame hajmi ≤ 256 KiB.

Rust’dagi aniq turlar: [`server/src/protocol.rs`](../server/src/protocol.rs). Dart klienti: [`packages/ecochain_rtc/lib/src/internal/signal_client.dart`](../packages/ecochain_rtc/lib/src/internal/signal_client.dart).

## 1. Konvert

| Yo‘nalish | Ko‘rinish |
| --- | --- |
| Klient → server so‘rov | `{"v":1,"type":"<so‘rov>","id":"<so‘rov id>","data":{...}}` |
| Muvaffaqiyatli javob | `{"v":1,"type":"response","id":"<so‘rov id>","ok":true,"data":{...}}` |
| Xato javobi | `{"v":1,"type":"response","id":"<so‘rov id>","ok":false,"error":{"code":"room_full","message":"...","retryable":false}}` |
| Server hodisasi | `{"v":1,"type":"<hodisa>","data":{...}}` (`id` yo‘q) |

* **`v`** — protokol versiyasi. Server faqat `1` ni qabul qiladi; boshqasiga `unsupported_version` javobi va `4000` close kodi qaytadi. Yangi versiya chiqqanda server bir muddat ikkala versiyani qo‘llaydi; klient o‘z versiyasini har frame’da yuboradi.
* **`id`** — klient tanlaydigan 1–64 belgili satr. Javob aynan shu `id` bilan keladi; javoblar so‘rovlar tartibida yuboriladi.
* **Xatoni aniqlash** — `ok:false` va `error.code`. Klient faqat `code` bo‘yicha qaror qiladi; `message` faqat log uchun (maxfiy ma’lumot bo‘lmaydi). `retryable:true` bo‘lsa, so‘rovni keyinroq takrorlash mumkin.

### Takroriy so‘rovlar (idempotentlik)

Server har bir sessiya uchun oxirgi 64 ta so‘rovning natijasini `id` bo‘yicha saqlaydi (`ping`dan tashqari). Bir xil `id` bilan qayta yuborilgan so‘rov qayta bajarilmaydi — birinchi natija qaytariladi. Bu reconnect’dan keyin javobi kelmay qolgan so‘rovni xavfsiz takrorlash imkonini beradi (masalan, `participant.remove`ning ikkinchi marta bajarilishi “unknown participant” xatosiga aylanmaydi).

Kesh resume’dan keyin ham saqlanadi, shuning uchun `id` butun sessiya davomida noyob bo‘lishi kerak: yangi socket’da hisoblagichni `r1`dan qayta boshlamang (SDK har socket uchun tasodifiy prefiks ishlatadi). Server natijani `id` va so‘rov mazmuni bo‘yicha taqqoslaydi: bir xil `id`, lekin boshqa mazmundagi so‘rov keshdagi javobni olmaydi, balki qayta bajariladi.

Bitta socket’dagi so‘rovlar qat’iy ketma-ketlikda bajariladi; bir xonadagi barcha so‘rovlar bitta xona aktori ichida navbat bilan qayta ishlanadi. Shuning uchun parallel join, chiqish va reconnect poygasi deterministik hal bo‘ladi.

## 2. Xato kodlari

| `code` | Ma’nosi | Klient nima qiladi |
| --- | --- | --- |
| `bad_request` | Noto‘g‘ri JSON yoki maydonlar | Dasturiy xato, qayta urinmaydi |
| `unsupported_version` | `v` qo‘llanmaydi | SDK’ni yangilash |
| `unauthorized` | Token noto‘g‘ri yoki imzo mos emas | Sessiya tugaydi |
| `token_expired` | Token muddati o‘tgan | `tokenProvider` orqali yangi token, keyin qayta join |
| `forbidden` | Token bu amalga ruxsat bermaydi (boshqa xona, mezbon emas) | Sessiya tugaydi / amal rad etiladi |
| `room_not_found` | Xona yo‘q, `auto_create` o‘chiq | Sessiya tugaydi |
| `room_full` | `max_participants`ga yetilgan | Sessiya tugaydi |
| `room_ended` | Mezbon uchrashuvni yakunlagan | Sessiya tugaydi |
| `not_joined` | `join`dan oldin boshqa so‘rov | Dasturiy xato |
| `invalid_state` | Joriy holatda mumkin emas (masalan, eskirgan offer’ga answer) | Odatda e’tiborsiz; yangi offer keladi |
| `limit_exceeded` | Masalan, video obunalar soni limitdan oshdi | Obunalar sonini kamaytirish |
| `unavailable` | Xona yopilmoqda yoki server band (`retryable:true`) | Backoff bilan qayta urinish |
| `internal` | Kutilmagan server xatosi (`retryable:true`) | Backoff bilan qayta urinish |

## 3. WebSocket close kodlari

| Kod | Sabab | Qayta ulanish |
| --- | --- | --- |
| `1000` | `leave`dan keyin normal yopilish | Yo‘q |
| `1001` | Server o‘chmoqda / xona yopilmoqda | Ha, backoff bilan |
| `4000` | Protokol xatosi | Yo‘q |
| `4001` | Shu identity’ning yangi ulanishi sessiyani oldi | Yo‘q (`replacedByOtherSession`) |
| `4003` | Avtorizatsiya xatosi | Yo‘q |
| `4004` | Uchrashuv yakunlandi | Yo‘q (`roomEnded`) |
| `4005` | Mezbon ishtirokchini chiqardi | Yo‘q (`removed`) |
| `4008` | `join` 10 soniyada kelmadi yoki heartbeat to‘xtadi | Ha |
| boshqa / close frame’siz uzilish | Tarmoq | Ha, resume bilan |

## 4. Token

JWT, `alg: HS256`. Tokenni Ecochain backend’i (masalan, Messenger) chiqaradi; media server faqat tekshiradi.

| Claim | Majburiy | Tavsif |
| --- | --- | --- |
| `iss` | ha | `auth.issuer` bilan teng bo‘lishi shart |
| `sub` | ha | Ishtirokchi identity’si (`[A-Za-z0-9_.:@-]{1,128}`) |
| `room` | ha | Token **faqat shu xonaga** kiritadi |
| `role` | ha | `host` yoki `participant` |
| `name` | yo‘q | Ko‘rinadigan ism |
| `can_publish` | yo‘q (true) | Mikrofon/kamera/ekran yuborish huquqi |
| `exp`, `iat` | ha | Unix soniyalar; ±30 s soat farqi qabul qilinadi |
| `nbf` | yo‘q | Shu vaqtgacha yaroqsiz |

`join.data.room` berilsa va tokendagi `room`dan farq qilsa — `forbidden`. Identity faqat tokendan olinadi; klient uni o‘zgartira olmaydi. Server token va kalitlarni logga yozmaydi (`Secret` turi `Debug`/`Display`da yashiriladi).

## 5. Sessiyalar: yangi ulanish va eski ulanish

* Xonada **bitta identity — bitta o‘rin**. Ishtirokchi `participant_id`si (`PA_...`) identity xonada turgan vaqtda o‘zgarmaydi.
* `join` javobida `session_id` (`SE_...`, maxfiy) keladi. Klient uni saqlaydi.
* **Resume:** yangi socket `join` ichida `resume_session_id` yuborsa va sessiya hali mavjud bo‘lsa, socket shu sessiyaga ulanadi: `resumed:true`, media peer’lar va obunalar saqlanadi, boshqalar ikkinchi `participant.joined` ko‘rmaydi (faqat `participant.updated` bilan `connected:true`). Agar eski socket hali ochiq bo‘lsa, unga `session.replaced` va `4001` yuboriladi.
* **Almashtirish:** shu identity `resume_session_id`siz (yoki noto‘g‘ri id bilan) kirsa, eski sessiya tugatiladi: eski socket’ga `session.replaced` + `4001`, uning trek’lari unpublish qilinadi, `participant_id` saqlanadi, yangi `session_id` beriladi (`resumed:false`).
* **Grace:** socket kutilmaganda uzilsa, ishtirokchi `reconnect_grace_secs` (standart 30 s) davomida o‘rnini va media peer’larini saqlaydi; boshqalarga `connected:false` yuboriladi. Muddat tugasa — `participant.left` (`reason:"timeout"`).
* **Heartbeat:** klient har `heartbeat_interval_ms` (5 s) da `ping` yuboradi. Server 15 s jim qolgan socket’ni `4008` bilan yopadi. `ping` xona taskiga bormaydi — band xona sog‘ klientni “o‘lik” deb hisoblamaydi.

## 6. So‘rovlar

| `type` | `data` | Javob `data` | Huquq |
| --- | --- | --- | --- |
| `join` | `token`, `resume_session_id?`, `room?`, `client?{sdk,platform}` | quyida | birinchi xabar |
| `ping` | – | `server_time_ms` | hamma |
| `leave` | – | `{}`, keyin socket `1000` | hamma |
| `publisher.offer` | `sdp`, `tracks[]`, `reset?` | `sdp` (answer) | `can_publish` (trek bo‘lsa) |
| `subscriber.answer` | `sdp`, `revision` | `{}` | hamma |
| `ice.candidate` | `target` (`publisher`/`subscriber`), `candidate` | `added` | hamma |
| `ice.restart` | `target:"subscriber"` | `{}`; keyin yangi `subscriber.offer` | hamma |
| `subscriber.reset` | – | `{}`; yangi subscriber peer va offer | hamma |
| `track.mute` | `source`, `muted` | `{}` | trek egasi |
| `subscription.update` | `changes[]: {track_id, subscribed, quality?}` | `{}` | hamma |
| `room.end` | – | `{}`, keyin hammaga `room.ended` + `4004` | `host` |
| `participant.remove` | `participant_id` | `{}`; shu identity bu xonaga, xona qayta yaratilsa ham, kira olmaydi (`forbidden`): chiqarilgan sessiya tokenlarining eng kech `exp`i (+30 s) yoki `removal_memory_secs` (standart 24 soat) — qaysi biri keyinroq bo‘lsa, shu vaqtgacha | `host` |

`join` javobi:

```json
{
  "protocol": 1,
  "session_id": "SE_...",
  "resumed": false,
  "participant": {"participant_id": "PA_...", "identity": "alice", "name": "Alice", "role": "host", "connected": true, "tracks": []},
  "participants": [ ... ],
  "room": {"room_id": "demo", "max_participants": 25, "max_video_subscriptions": 9},
  "speakers": [],
  "ice_servers": [{"urls": ["stun:..."]}],
  "reconnect_grace_ms": 30000,
  "heartbeat_interval_ms": 5000
}
```

`publisher.offer.tracks[]`: `{mid, source, muted, layers}`. `source`: `microphone | camera | screen | screen_audio`. `layers` — simulcast rid’lari pastdan yuqoriga (`["q","h","f"]`), faqat video uchun, ≤3. Ro‘yxatda yo‘q trek unpublish qilinadi.

## 7. Server hodisalari

| `type` | `data` |
| --- | --- |
| `participant.joined` | `participant` |
| `participant.updated` | `participant` (trek, mute, `connected` o‘zgardi) |
| `participant.left` | `participant_id`, `reason` (`left`, `timeout`, `removed`) |
| `subscriber.offer` | `sdp`, `revision`, `tracks[]: {mid, track_id, participant_id, source}` |
| `speakers.changed` | `speakers[]: {participant_id, level}` (eng balanddan, `rooms.max_forwarded_audio` tagacha, standart 3). Faqat uzatilayotgan va darajasi ≥ −30 dBov bo‘lgan gapiruvchilar kiradi; bo‘sh slot orqali uzatilayotgan past ovozli (−70…−30 dBov) audio ro‘yxatga kirmaydi va hodisa keltirmaydi. Hodisa kamida 250 ms oralig‘ida yuboriladi; gapiruvchi jim bo‘lgach ham 500 ms (ushlab turish) ro‘yxatda qoladi |
| `peer.failed` | `target` — server tomonda peer o‘ldi; klient uni qayta quradi |
| `room.ended` | `reason` (`ended_by_host`, `ended_by_admin`) |
| `removed` | `reason` |
| `session.replaced` | `reason` |

## 8. Media muzokarasi

Har ishtirokchida ikkita PeerConnection bor:

1. **Publisher** — klient offer qiladi, faqat yuboradi. Birinchi offer’da `ecochain-pub` data channel bor (trek bo‘lmasa ham ICE/DTLS tayyor turadi). Kamera yoqilganda transceiver qo‘shiladi va yangi `publisher.offer`; keyingi o‘chirish/yoqish `replaceTrack` + `track.mute` bilan, qayta muzokarasiz.
2. **Subscriber** — server offer qiladi, faqat yuboradi. Klient `subscriber.answer`ni aynan shu `revision` bilan qaytaradi. Server bir vaqtda faqat bitta javobsiz offer ushlaydi; oradagi o‘zgarishlar keyingi offer’ga yig‘iladi. 10 s javob kelmasa, offer qayta yuboriladi. Eskirgan revision’ga `invalid_state`.

Yo‘nalishlar ajratilgani uchun “glare” (ikki tomon bir vaqtda offer qilishi) bo‘lmaydi.

**Obunalar:** boshqa ishtirokchilarning audiolari avtomatik keladi (`subscription.update`da `subscribed:false` bilan o‘chirish mumkin). Video va ekran faqat `subscription.update` orqali, `max_video_subscriptions` limiti bilan. `quality`: `low | medium | high`.

**ICE:** server SDP’da host kandidatlarini beradi; klient kandidatlarini `ice.candidate` bilan yuborishi ixtiyoriy (server peer-reflexive manzilni STUN’dan ham biladi). Server faqat UDP kandidatlarini qabul qiladi; unspecified, multicast, broadcast manzillar va production’da global marshrutlanmaydigan manzillar (loopback, xususiy, link-local, CGNAT `100.64/10`, `198.18/15`, `240/4`, IPv6 ULA/site-local; IPv6 ichidagi IPv4 — mapped, compatible, NAT64, 6to4 — ichki manzil bo‘yicha baholanadi) rad etiladi (`added:false`), har peer uchun ko‘pi bilan 32 ta. Bu serverni ixtiyoriy manzilga STUN yuborishga majburlashning (SSRF) oldini oladi. Publisher ICE restart — `createOffer({iceRestart:true})` + `publisher.offer`. Subscriber ICE restart — `ice.restart`. To‘liq media reconnect — `publisher.offer` `reset:true` va `subscriber.reset`.

## 9. Misol oqim

```text
C → {"v":1,"type":"join","id":"r1","data":{"token":"eyJ..."}}
S → {"v":1,"type":"response","id":"r1","ok":true,"data":{"session_id":"SE_x","resumed":false,...}}
S → {"v":1,"type":"subscriber.offer","data":{"sdp":"v=0...","revision":1,"tracks":[]}}
C → {"v":1,"type":"subscriber.answer","id":"r2","data":{"sdp":"v=0...","revision":1}}
C → {"v":1,"type":"publisher.offer","id":"r3","data":{"sdp":"v=0...","tracks":[{"mid":"1","source":"microphone","muted":false,"layers":[]}]}}
S → {"v":1,"type":"response","id":"r3","ok":true,"data":{"sdp":"v=0..."}}
S → (boshqalarga) {"v":1,"type":"participant.updated","data":{"participant":{...}}}
```

## 10. Workplace collaboration and processing

Admitted join responses include `capabilities` (`work_tools_v1`, optional
`recording_v1`, optional `captions_v1`), `meeting` snapshot, recording/caption
status, and optional instance-scoped `archive_token`/host `archive_manage_token`.
Pending lobby joins instead return `waiting:true`/`waiting_id`; no workspace,
archive token or media peers are created. `lobby.admitted` carries the normal join
payload; `lobby.changed` lists waiting entries to hosts only.

`meeting.snapshot {}` returns canonical state. `meeting.action {action:{type,...}}`
is authorized by server identity/role and returns a revision patch for durable
mutations. `meeting.changed` broadcasts the same patch:

```json
{"instance_id":"uuid","revision":12,"patch":{"set":{"presentation":null},"collections":{"notes":{"upsert":[{"id":"n1","text":"Updated","revision":2}],"remove_ids":[]}}}}
```

Collections merge each partial upsert by `id` (`hands` uses `participant_id`).
Unmentioned fields remain intact; object deletion does not resend old point arrays.
The SDK ignores stale patches and requests a full snapshot on a revision gap.
Durable replies follow successful fsync. Maximum snapshot is 10 MiB, action 32 KiB.

Action families: `presentation.begin/end/pause/offer/decline/policy`, `drawing.policy`,
`board.open/page`, `object.add/remove/restore/clear`, `hand.set`,
`poll.create/vote/close`, `question.add/vote/answer`, `agenda.put/start`, `note.put`,
`decision.put`, `task.put`, `feedback.set`, lobby/room policy and host mute.
Objects use normalized `[x,y]` point arrays in 0..1, integer ARGB colors and
normalized widths. Server owns author, presentation IDs and object revisions.
`meeting.pointer` (20/s, TTL) and `meeting.reaction` (5/s) are ephemeral.

Host `recording.start/stop` and `captions.start/stop` run independent workers;
`recording.changed`/`captions.changed` broadcast public job status. Live
`captions.segment` contains `start_ms`, `end_ms`, known `participant_id`, `name`,
and `text`. Unsupported/unconfigured processing returns an explicit error.

Archive HTTP endpoints (bearer `archive_read`, admitted occurrence membership):

- `GET /v1/archives/{instance}`: workspace, members, provider capabilities and jobs.
- `GET /v1/archives/{instance}/files/{file}`: authenticated image/recording, supports
  one byte Range including suffix requests; invalid ranges return 416.
- `POST .../images`: binary PNG/JPEG ≤5 MiB, requires **live** JWT + drawing rights.
- `POST .../playback-link {recording_id}`: one-use 60-second handoff URL; fragment
  contains a nonce, never a long-lived JWT. Player exchanges nonce at
  `POST .../playback-session {handoff}` for an occurrence-scoped HttpOnly cookie.
- Admin `POST /v1/archive-tokens {instance_id,identity,ttl_secs}` issues read access
  after membership verification. RTC and archive token keys/issuers are separate.

Host management bearer `archive_manage` is additionally required for:

- `POST .../transcripts {recording_id}`: idempotent recording transcript job.
- `POST .../summaries {transcript_job_id?}`: draft from a ready transcript or notes.
- `POST .../summaries/confirm {confirmed:true,confirmation:{job_id,expected_revision,draft}}`:
  atomic reviewed summary/decisions/tasks, revision conflicts return 409.
- `GET .../jira/projects`, `POST .../jira/preview` with task_id, project_key,
  summary, description, optional assignee_identity/due_date.
- `POST .../jira/{job_id}/submit {confirmed:true,preview_hash}`: hash binds reviewed
  payload **and origin**. Pending/uncertain retries reconcile only and do not repeat
  POST. Ready/failed jobs and uncertain status remain visible in archive results.

See [workplace setup](WORK_MEETING_BACKEND.md) for limits and platform boundaries.
