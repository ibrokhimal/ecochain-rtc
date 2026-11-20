# Arxitektura

```text
 Flutter ilova (Meet / Messenger)
   └─ ecochain_meeting_ui   MeetingView, JoinPanel (umumiy ekran)
        └─ ecochain_rtc     RtcSession, RtcVideoView (SDK, ekranlarsiz)
             ├─ SignalClient ── WSS ──────────────┐
             └─ flutter_webrtc (libwebrtc)        │
                  ├─ publisher PC ── UDP/DTLS-SRTP ┼──┐
                  └─ subscriber PC ─ UDP/DTLS-SRTP ┼──┤
                                                   ▼  ▼
 Rust server (ecochain-rtc-server)
   http.rs ─ axum: /v1/rtc (WS), /v1/rooms, /v1/tokens, /metrics, /healthz
   signaling.rs ─ har socket uchun task: join, token, so‘rovlarni xonaga uzatish
   rooms.rs ─ registry: xona id → RoomHandle; yakunlangan xonalar xotirasi
   room/ ─ har xona uchun BITTA tokio task (aktor):
        participant.rs  ishtirokchi, sessiya, javoblar keshi
        mod.rs          join/resume/almashtirish, huquqlar, grace, tozalash
        media.rs        str0m peer’lar, SDP, forwarding, simulcast, speaker
   sfu/udp.rs ─ umumiy UDP port; STUN ufrag → xona marshruti; yuborish thread’i
   sfu/peer.rs ─ str0m::Rtc o‘rami
   sfu/layers.rs ─ simulcast qatlam tanlash, RTP vaqt uzluksizligi
   auth.rs, config.rs, metrics.rs, protocol.rs, bot.rs (test klienti)
```

## Asosiy qarorlar

### Rust WebRTC: `str0m` 0.25

* **Nega:** sans-IO dizayn — kutubxona socket va thread’larni o‘zi boshqarmaydi. Bu SFU uchun qulay: bitta xonaning barcha peer’lari bitta task ichida, lock’siz. ICE, DTLS-SRTP, NACK/RTX, TWCC BWE, simulcast (RID), Opus/VP8/VP9/H264/AV1 qo‘llanadi. Litsenziya MIT OR Apache-2.0.
* **Kriptografiya backend’i:** `aws-lc-rs` (str0m standarti). `examples` feature o‘chirilgan.
* **Cheklovlar:** TURN server o‘zida yo‘q (coturn kabi alohida xizmat kerak; vaqtinchalik ma’lumotlar uchun quyidagi “TURN (coturn)” bo‘limiga qarang); TCP/ICE-TCP yo‘q (faqat UDP). str0m “frame mode”da ishlatiladi: media birliklari depaketlanadi va har subscriber uchun qayta paketlanadi — bu simulcast almashishini xavfsiz qiladi, lekin passthrough’dan ko‘ra ko‘proq CPU sarflaydi.
* **Alternativalar:** `webrtc-rs` (Pion porti) — og‘irroq, async-ga bog‘langan, SFU yozish uchun ko‘proq kod. LiveKit — Go’da, o‘z SDK’lari bilan; vazifa o‘z serverimizni talab qiladi.

### Ikki PeerConnection (publisher + subscriber)

Yo‘nalishlar ajratilgan: publisher’da faqat klient offer qiladi, subscriber’da faqat server. Glare yo‘q, har birini alohida qayta qurish mumkin (masalan, faqat subscriber’ni `subscriber.reset`). LiveKit ham shu modelni ishlatadi.

### Xona aktori

Har xona — bitta tokio task, unga signaling va UDP dispatcher faqat kanal orqali murojaat qiladi. Afzallik: parallel join/leave/resume poygalari yo‘q, ishtirokchi ikki nusxada paydo bo‘lishi strukturaviy jihatdan mumkin emas (identity bo‘yicha bitta o‘rin). Kamchilik: bitta xona bitta CPU yadrosi bilan cheklangan (yuklama natijalariga qarang). UDP yuborish alohida OS thread’ga chiqarilgan: profilda xona taskining ~40% vaqti `sendto`da edi.

### UDP marshrutlash

Barcha xonalar bitta UDP portni (standart 7882) ishlatadi. Notanish manzildan kelgan STUN so‘rovidagi `USERNAME` (`server_ufrag:client_ufrag`) orqali xona topiladi va manzil eslab qolinadi; keyingi DTLS/SRTP shu manzil bo‘yicha boradi. Wi‑Fi → mobil tarmoq almashganda yangi manzil yana STUN bilan boshlanadi, shuning uchun roaming alohida kod talab qilmaydi. Serverning o‘z tarmog‘i o‘zgarsa (`media.listen` bo‘sh, `media.follow_primary = true` standart): asosiy IP har 3 s tekshiriladi, avtomatik socket yangi manzilga ko‘chiriladi, xonalar `watch` kanali orqali xabar olib, har peer’da host candidate’ni almashtiradi; klient ICE restart qilganda yangi manzilni oladi.

### Simulcast va tarmoqqa moslashish

Kamera 3 qatlamda yuboriladi (`q` ¼, `h` ½, `f` to‘liq; 150/500/1500 kbit/s). Har subscriber uchun server bitta qatlamni tanlaydi: so‘ralgan sifat (tile o‘lchami) va str0m TWCC bandwidth bahosi asosida. Ustuvorlik qat’iy: avval ekran ulashish, keyin `high` (mahkamlangan) va h.k.; har trek kamida eng past qatlamni oladi. Qatlam almashishi faqat keyframe’da, RTP vaqt shkalasi qayta yoziladi (`sfu/layers.rs::Timeline`).

### Paket yo‘qolishi

* NACK/RTX — str0m publisher tomonda yo‘qolgan paketlarni so‘raydi, subscriber tomonda qayta yuboradi.
* Keyframe so‘rovi (PLI) — kadr uzilib qolsa, yangi video obunada, qatlam almashganda; publisher trek’iga 500 ms’da bir martadan ko‘p emas.
* Opus in-band FEC — libwebrtc standarti.

### Qurilmada qabul qilinadigan video soni

`max_participants` (xonaga kirish, standart 25) va `max_video_subscriptions` (bitta qurilma bir vaqtda qabul qiladigan video, server limiti 9) alohida. SDK standarti: mobil 4, desktop 9. `RtcVideoSubscriptionPolicy`: ekran ulashish → mahkamlangan → faol gapiruvchilar → ko‘rinib turgan tile’lar.

## Flutter SDK hayot sikli

* `RtcSession` UI’dan mustaqil, ilova darajasida saqlanadi (`MeetingController`). Ekran yopilsa ham qo‘ng‘iroq davom etadi.
* **Generation hisoblagich:** har `connect` va `leave` `_generation`ni oshiradi. Har asinxron davomiylik (getUserMedia, createPeerConnection, join javobi) natijani qo‘llashdan oldin generation’ni tekshiradi; eskirgan bo‘lsa, yaratilgan resursni darhol yopadi. Shu sababli chiqishdan keyin kelgan javob sessiyani qayta tiriltirmaydi.
* **Navbat (`Serial`):** media amallari ketma-ket bajariladi — tez-tez bosish ikkinchi kamera ochmaydi, ikkita `createOffer` ustma-ust tushmaydi.
* **Takroriy `connect`:** jarayondagi future qaytariladi; ikkinchi socket ochilmaydi (unit test bilan tekshirilgan).
* **Qayta ulanish:** socket uzilsa → `reconnecting`, backoff (0,5 s dan 8 s gacha, ×2, equal jitter), `resume_session_id` bilan join. To‘xtash shartlari: muvaffaqiyat, fatal kod (`unauthorized`, `forbidden`, `room_full`, `room_ended`, close `4001/4004/4005`), `giveUpAfter` (60 s), `leave`. `token_expired` — `tokenProvider` bo‘lsa yangi token olinadi.
* **ICE monitoring:** `disconnected/failed` 2 s davom etsa ICE restart, ikki urinishdan keyin ikkala peer qayta quriladi.
* **Foydalanuvchi tanlovi:** mikrofon/kamera holati (`_wantMicrophone`, `_wantCamera`) reconnect’dan keyin qayta qo‘llanadi.
* **Tozalash (`leave`/`_end`):** socket yopiladi, media navbati bo‘shatiladi, ikkala PC `close()+dispose()`, lokal trek’lar `stop()`, stream’lar `dispose()`, remote render stream’lar `dispose()`, taymerlar bekor qilinadi.

## Shifrlash va ruxsatlar

* Signaling/API: TLS (`http.tls_cert/tls_key` yoki `behind_tls_proxy`). Production rejimida TLS’siz server ishga tushmaydi, inline secret’lar rad etiladi, `/v1/dev/token` 404, `/metrics` admin kaliti bilan.
* Media: DTLS-SRTP (WebRTC majburiy). Server media’ni ochiq ko‘radi — SFU va kelajakdagi yozib olish uchun bu talab qilingan. Bu end-to-end shifrlash emas.
* SDK `wss://` talab qiladi; `ws://` faqat `allowInsecureLocalhost` va loopback/xususiy LAN manzil uchun. Sertifikat tekshiruvini o‘chirish imkoniyati SDK’da umuman yo‘q. Android release build’da cleartext o‘chiq (faqat debug manifest’da yoqilgan).
* Kamera/mikrofon faqat foydalanuvchi tanlovi bilan yoqiladi (`microphone/camera: false` standart).

## Keyingi bosqich: yozib olish va fayllar (hali bajarilmagan)

Rejalashtirilgan dizayn: server SFU’dan trek’larni yozadi (RTP → WebM/Opus+VP8), har fayl alohida tasodifiy kalit (AES-256-GCM) bilan shifrlanadi; fayl kaliti KMS/master kalit bilan o‘raladi (envelope encryption). Ko‘rish/yuklab olish `GET /v1/files/{fileId}` orqali, server token’dagi identity/xona bo‘yicha aynan shu `fileId`ga ruxsatni tekshiradi. Mobil SDK’ga ta’siri: audio-video qo‘ng‘iroq API’si o‘zgarmaydi; yozib olish holati uchun `room.recording` hodisasi va fayl ro‘yxati API’si qo‘shiladi.

## Kengaytirish

25 kishilik bitta xona hozir bitta yadro bilan chegaralangan (natijalar `TESTING.md`da). Bir server ichida: subscriber egress’ni (SRTP shifrlash + paketlash) bir nechta thread’ga taqsimlash. Serverlararo kaskad (xonani bir necha SFU’ga bo‘lish) o‘lchovlar shuni talab qilgandagina; hozirgi raqamlar bitta xona uchun bunga hali zarurat ko‘rsatmaydi, ko‘p xonalar esa allaqachon turli yadrolarda ishlaydi.

## TURN (coturn)

SFU faqat UDP’da ishlaydi; UDP bloklangan tarmoqdagi mijozlar coturn orqali TCP/TLS 443 bo‘ylab ulanadi. Statik parol tarqatilmaydi: har bir `join` javobiga coturn `use-auth-secret` (TURN REST API) bo‘yicha vaqtinchalik ma’lumot qo‘shiladi — `username = "<muddati>:<identity>"`, `credential = base64(HMAC-SHA1(secret, username))`. Statik `media.ice_servers` o‘zgarmaydi, TURN yozuvi oxiriga qo‘shiladi.

O‘rnatish:

1. Alohida xostga coturn o‘rnating va `server/deploy/coturn/turnserver.conf.example` ni `/etc/turnserver.conf` ga nusxalab, `<...>` o‘rinlarini to‘ldiring (`static-auth-secret` — `openssl rand -base64 32`). `allowed-peer-ip` faqat SFU IP’sini qoldiradi, xususiy diapazonlar `denied-peer-ip` bilan yopiladi.
2. SFU konfiguratsiyasida `[media.turn]` ni yoqing (`config/production.example.toml`): `urls` (`turn:`/`turns:`), `secret_env` (production’da inline secret taqiqlangan, coturn bilan bir xil qiymat), `ttl_secs` (standart 3600, eng kami 60).
3. Xavfsizlik devori: TCP/UDP 3478, TCP 443 (TURN) ochiq; coturn → SFU UDP `media.udp_port`.
4. Secret va hosil bo‘lgan credential hech qachon loglanmaydi.
