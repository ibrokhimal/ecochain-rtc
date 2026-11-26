# Sinov natijalari

Sana: 2026-11-18 (100 kishilik bo‘lim: 2026-11-20). Mashina: Apple Silicon Mac (10 yadro), macOS 26, Rust 1.96, Flutter 3.44.1 / Dart 3.12.1.

## Adaptiv simulcast tekshiruvi — 2026-11-26

Yangi tekshiruvlar `server/src/sfu/{layers,adaptation}.rs`,
`server/src/room/video.rs` va `server/src/room/adaptation_tests.rs`da.

- Budjet: 15% transport zaxirasi, eligible audio uchun 64 kbit/s, boshlang‘ich
  800 kbit/s, nol/katta qiymatlar, priority va qatlam yuqori chegarasi.
- Fake vaqt bilan 2 s / 20% recovery gate, zaxira yetmay qolishi, yetarli
  zaxira bilan kichik TWCC pasayishlari, track ID qayta tartiblanishi.
- Haqiqiy ICE + DTLS-SRTP: bitta q/h/f publisher va 300/800/2500 kbit/s
  **test-only baholar** bilan uch receiver. Qabul qilingan synthetic qatlam
  marker’lari q/h/f ekanligi tekshiriladi; boshqa qatlamga kadr sizmaydi.
  Faqat bittasida video pauza/resume, audio va qolgan receiver’lar davom etadi.
- Single-encoding kamera pauzadan yangi keyframe bilan qaytadi; qo‘shimcha
  `subscriber.offer` bo‘lmaydi. Forwarding gate single-encoding (RID yo‘q)
  va oldingi RID’ga qaytishda delta frame’ni keyframe’dan oldin o‘tkazmaydi.
- **Haqiqiy TWCC sinovi:** test-only UDP relay receiver downlink’ini
  4 Mbit/s → 120 kbit/s → 4 Mbit/s qiladi. 150 ms navbat sig‘maganda
  SRTP/padding paketlari tushadi. Bu sinovda baho inject qilinmaydi:
  server TWCC bahosi 250 kbit/s’dan pastga tushadi va video pauza bo‘ladi;
  audio hamda oddiy yo‘ldagi ikkinchi receiver ishlashda davom etadi.
  Cheklov olinganda yangi keyframe bilan video, keyin `Layer(2)` tiklanadi.
  Alohida muvaffaqiyatli run’da tiklangan baho 2841 kbit/s bo‘ldi.

Relay faqat downlink SRTP’ni cheklaydi; ICE/DTLS/RTCP va uplink erkin.
Bu real internetning barcha sharoitlarini (TURN, Wi-Fi radio, ikki tomonlama
loss, mobil CPU) modellashtirmaydi. Synthetic VP8 kadrlar SFU, RTP va
qatlam tanlovini tekshiradi; haqiqiy kamera dekoderi tasvirini isbotlamaydi.
Ushbu o‘zgarish uchun telefon/brauzerda qo‘lda tasvir sinovi bajarilmadi.

Oldin yuborilgan kadrlar RTX yoki qabul qiluvchi buferidan pauzadan keyin
ham kelishi mumkin. Shaped test serverning yangi video yozish hisoblagichi
pauzada o‘zgarmasligini tekshiradi; qabul qilingan kech kadrlarni yangi
forwarding bilan adashtirmaydi. Test override, relay va hisoblagichlar
`cfg(test)` ostida: production binary, HTTP yoki signaling’da mavjud emas.

Yangi 5 ta room adaptation testi birgalikda o‘tdi (42.48 s); mavjud
`tests/media.rs` 14/14 o‘tdi (63.57 s). Single-encoding startup sinovlarida
TWCC probing + recovery uchun 20 s gacha vaqt ajratildi; leaver testi avval
video kelganini tekshiradi. Bu real qurilmada o‘lchangan latency SLA emas.

Socket’li testlar sandbox tashqarisida lokal UDP/TCP ruxsati bilan ishlatildi.
Timing uchun alohida synthetic xonalar CPU’ni bo‘lishmasligi uchun to‘liq
tekshiruv `cargo test -- --test-threads=1` bilan bajariladi.

## Buyruqlar

```bash
cd server && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
cargo test --test resilience -- --nocapture      # uzilish vaqtlarini ko‘rish uchun
cd packages/ecochain_rtc && dart format --set-exit-if-changed lib test && flutter analyze && flutter test
cd packages/ecochain_meeting_ui && flutter analyze && flutter test
cd apps/meet && flutter analyze && flutter test
cd apps/messenger_demo && flutter analyze && flutter test
```

## Avtomatik testlar

| To‘plam | Soni | Natija | Nimani ushlaydi |
| --- | --- | --- | --- |
| Rust unit (`cargo test --lib`) | 18 | ✅ | Token imzosi/muddati/`alg:none`/payload almashtirish; konfiguratsiya (production’da inline secret va TLS’siz ishga tushmaydi, secret logga chiqmaydi); protokol parse va versiya xatosi; STUN ufrag; simulcast qatlam taqsimoti; RTP vaqt uzluksizligi; javob keshi |
| `tests/signaling.rs` | 9 | ✅ | Ruxsatsiz kirish (noto‘g‘ri imzo, boshqa xona tokeni, muddati o‘tgan token, `join`siz so‘rov); xona to‘lishi (auto va admin API `max_participants`); takroriy ulanish (eski sessiya `4001`, ishtirokchi ikki marta ko‘rinmaydi); resume va grace tugaganda tozalash; mezbon yakunlashi va kech kirishga `room_ended`; mezbon chiqarishi va bir xil `id` bilan takroriy so‘rov; chiqishdan keyin metrikalar 0 |
| `tests/media.rs` | 3 | ✅ | Haqiqiy ICE + DTLS-SRTP + SFU orqali audio/video; simulcast publisher’dan faqat bitta qatlam (~25 fps) uzatilishi; kamerasiz ishtirokchi |
| `tests/resilience.rs` | 4 | ✅ | 5 s va 15 s to‘liq uzilish, tarmoq almashishi (yangi manzil + ICE restart), signaling uzilishi (media to‘xtamaydi), ishtirokchi takrorlanmaydi |
| `ecochain_rtc` | 15 | ✅ | Backoff/jitter chegaralari; obuna ustuvorligi; signaling id moslash, xato kodlari, timeout, close kodi, heartbeat; `ws://` faqat lokal manzilga; rad etilgan join sababi; parallel `connect` bitta urinish va bitta socket; `connect` paytida `leave` — kech javob sessiyani tiriltirmaydi va socket yopiladi; faol bo‘lmagan sessiyada mikrofon yoqilmaydi |
| `ecochain_meeting_ui` | 2 | ✅ | Meet ekranini yopib/qayta ochish sessiyaga tegmaydi; tugash sababi ko‘rsatiladi |
| `apps/meet`, `apps/messenger_demo` | 1 + 1 | ✅ | Join formasi; chat xabarlari uchrashuv ekrani ochilib-yopilganda saqlanadi |

## Uzilishlar va audio tiklanish vaqti

Headless str0m bot klientlari, loopback, haqiqiy server (`tests/resilience.rs`). Vaqt — tarmoq qaytganidan keyin uzilishdan keyin yuborilgan birinchi audio kadr kelguncha.

| Ssenariy | Alice → Bob | Bob → Alice | Izoh |
| --- | --- | --- | --- |
| 5 s to‘liq uzilish (UDP va WebSocket) | 299 ms | 284 ms | Socket tirik qoldi, resume kerak bo‘lmadi |
| 15 s to‘liq uzilish | 297 ms | 294 ms | Server heartbeat bo‘yicha socket’ni yopdi, klient resume qildi (`resumes=1`), ishtirokchi bitta |
| Tarmoq almashishi (yangi UDP manzil, ikkala peer’da ICE restart) | 314 ms | 316 ms | |
| Faqat signaling uzilishi | 0 (to‘xtamadi) | 0 | Resume paytida audio uzluksiz keldi |

**Haqiqiy Flutter ilovalari bilan** (macOS Meet + iOS Simulator Messenger): server 5 s o‘chirib qayta yoqildi. Ikkala ilova 1–2 s ichida qayta join qildi, ikkala yo‘nalishda media server qayta ishga tushgandan **6,9 s** keyin oqmoqda edi (backoff + yangi PeerConnection’lar; serverda holat yo‘qolgani uchun bu to‘liq reconnect yo‘li). Mikrofon holati saqlandi, ishtirokchilar ikki nusxada ko‘rinmadi.

**Tekshirilmagan:** haqiqiy telefonda Wi‑Fi ↔ LTE almashishi, real radio yo‘qotishi bilan sinov. Yuqoridagi tarmoq almashishi loopback’da simulyatsiya qilingan.

## Ikki ilova o‘rtasida haqiqiy audio-video

Haqiqiy Rust server (`config/development.toml`, release build):

1. macOS’da `apps/meet` (Alice, mikrofon + kamera, 3 simulcast qatlam) — serverga ~118 media birligi/s keldi.
2. iOS Simulator’da `apps/messenger_demo` (Bob, mikrofon; simulatorda kamera yo‘q).
3. Bob ekranida Alice’ning **haqiqiy kamera videosi** ko‘rindi. Server: kirish 161/s, chiqish 132/s — Alice audio+video Bob’ga, Bob audio Alice’ga.
4. Messenger’da “Chatga qaytish” → xabar yozildi; shu paytda chiqish oqimi 134/s davom etdi, yangi join bo‘lmadi.
5. “Uchrashuvga qaytish” — aynan shu sessiya ko‘rsatildi (ikki ishtirokchi, video davom etmoqda).
6. Bob “Chiqish”: server `participant left reason="left"`, Bob socketi yopildi, chatdagi faol uchrashuv paneli yo‘qoldi. macOS ilova yopilgach, server grace (30 s) tugashi bilan ishtirokchini olib tashladi.

Ikkala mikrofon bitta Mac’dan foydalangani uchun audioni quloq bilan baholash o‘tkazilmadi; audio uzatilishi server hisoblagichlari bilan tasdiqlandi.

## Linux server, Wi‑Fi orqali (2026-11-18)

Server: Ubuntu 24.04, x86_64, 16 yadro, Wi‑Fi `172.20.10.3` (iPhone hotspot). Klientlar: Mac `172.20.10.14` (macOS Meet + iOS Simulator Messenger).

| Tekshiruv | Natija |
| --- | --- |
| `cargo test` Linux’da | 37/37 ✅ |
| Release build | 1 daq 51 s; server RSS ~18 MB, 2 ishtirokchida CPU ~2% |
| 2 bot Mac → Linux, Wi‑Fi | audio 100%, video 100% |
| macOS Meet → iOS Messenger | Alice kamera videosi Bob ekranida ko‘rindi |
| Audio ikki yo‘nalishda | kirish 100/s, chiqish 100/s (har biri ikkinchisining audiosini oladi) |
| Server 5 s o‘chirib yoqildi | ikkala ilova o‘zi qayta ulandi, media **7,5 s** da tiklandi |

### Wi‑Fi orqali yuklama va UDP yo‘qotishi

Ikkala qurilma bitta iPhone hotspot’ida: trafik Mac → iPhone → Linux yo‘lidan o‘tadi. O‘lchangan o‘tkazuvchanlik (10 MB `scp`): Linux → Mac **2,1 Mbit/s**, Mac → Linux **4,1 Mbit/s**; ping 6–560 ms, Linux → Mac 15% yo‘qotish. Linux’da Wi‑Fi power management yoqilgan (`iwconfig`: `Power Management:on`).

`ecochain_rtc_udp_dropped_total` endi sabab bo‘yicha ajratilgan (`send_queue_full`, `send_error`, `unrouted`, `room_queue_full`, `no_peer`). Oldingi o‘sish (5471 gacha) deyarli butunlay `send_error` edi:

* `EAGAIN` (os 11): Wi‑Fi qisqa to‘xtaganda ~200 KB lik standart socket yuborish buferi to‘lib, keyingi paketlar tashlanardi. **Tuzatildi:** media socket’lar 4 MiB bufer so‘raydi (kernel `net.core.wmem_max` gacha cheklaydi). Faqat audio, 2 bot: yetkazish **43% → 100%**, `send_error` 0.
* `EINVAL` (os 22): ICE loopback socket’idan LAN manzilga tekshiruv yuborardi. Endi bunday juftliklar o‘tkazib yuboriladi.

Video bilan bot sinovlari bu tarmoqda o‘lchov emas: har bot simulcast’da ~2,15 Mbit/s yuboradi (BWE’ga moslashmaydi), bu hotspot sig‘imidan katta. Botlar audio 5–75% oladi, server esa yuklama ostida emas (loopback’da xuddi shu yuklama 100%). Haqiqiy klientlar TWCC/BWE bilan sifatni pasaytiradi. Yuklama sinovini router orqali LAN’da yoki kabelda o‘tkazish kerak. Linux’da power save’ni o‘chirish tavsiya etiladi (`sudo iwconfig wlp0s20f3 power off` yoki NetworkManager’da `wifi.powersave = 2`).

Boshqa Wi‑Fi’da (router, server `172.16.137.58`, Mac `172.16.137.69`, ping 6–151 ms, 0% yo‘qotish), botlar Mac → Linux, 15 s:

| Bot / video | Audio | Video | Qabul, kbit/s (har bot) |
| --- | --- | --- | --- |
| 2 / 0 | 99% | — | 32 |
| 2 / 1 | 100% | 100% | 1 563 |
| 4 / 3 | 100% | 100% | 3 571 |
| 6 / 4 | 100% | 100% | 5 598 |
| 10 / 4 | 100% | 98% | 5 671 (jami ~57 Mbit/s) |

`send_error` va `send_queue_full` 0. Demak hotspot’dagi yo‘qotish tarmoq sig‘imi edi, server emas.

Tarmoq almashganda: `media.listen` bo‘sh bo‘lsa, server asosiy IP’ni har 3 s tekshiradi va o‘zgarsa media socket’ni yangi manzilga (o‘sha port) ko‘chiradi, barcha peer’larga yangi host candidate qo‘shib, eskisini bekor qiladi. Qayta ishga tushirish kerak emas. Mavjud qo‘ng‘iroqlar faqat klientlar serverga nom orqali ulangan bo‘lsa tiklanadi (`ubuntu.local`, DNS): IP bilan ulangan klient eski manzilga signaling ham qila olmaydi, yangi klientlar esa yangi IP bilan darrov ulanadi. Test: `tests/resilience.rs::server_address_change_moves_media` (loopback → LAN IP, ICE restart’dan keyin audio ~330 ms da tiklandi). Shu test subscriber ICE restart’dagi eski xatoni ham topdi: yangi ufrag faqat answer qabul qilingandan keyin kuchga kiradi, server esa uni offer paytida ro‘yxatdan o‘tkazardi, shuning uchun yangi manzildan kelgan STUN marshrutlanmasdi (`unrouted`). Endi ufrag answer’dan keyin ham yangilanadi. Yangi IP: serverda `hostname -I` yoki `ssh user01@ubuntu.local hostname -I`.

### Real Android qurilma (2026-11-18)

Huawei CTR-L91 (Android 12) + iOS Simulator, Mac’dagi server orqali (`UIC_Igov` Wi‑Fi telefon va Linux serverni bir-biridan ajratadi: ARP `FAILED`, Linux power save o‘chirilgandan keyin ham). Telefon kamerasi simulyatorda ko‘rindi.

* Telefon videosi qabul qiluvchilarda yonboshlab ko‘rinardi. Sabab: kamera kadrlari burilmagan holda, burilish esa RTP `urn:3gpp:video-orientation` kengaytmasida yuboriladi; server kadrlarni qayta paketlaganda bu qiymatni tashlab yuborardi. **Tuzatildi:** har trekning oxirgi orientatsiyasi saqlanadi va har uzatilgan kadrga yoziladi (o‘rtadan qo‘shilgan qabul qiluvchilar uchun ham).

Sinov paytida topilgan va tuzatilgan muammolar:

* macOS (va iOS qurilmalar) LAN’dagi serverga **Local Network** ruxsatisiz ulana olmaydi (`No route to host`, errno 65). Ilovalarga `NSLocalNetworkUsageDescription` qo‘shildi.
* Ulanishdan keyin kamera ochilmasa (`NotAllowedError`), xato faqat `lastError`da qolib, ekranda ko‘rinmas edi. Endi Meet ekranida qizil banner va logda xabar chiqadi.
* macOS’da ruxsat oynasi ochiq turganda Flutter UI thread bloklanadi va subscriber offer’ga javob kechikadi. Server 10 s dan keyin offer’ni **boshqacha** yangisi bilan almashtirardi, klient esa birinchisini qo‘llab bo‘lgan edi. Natijada WebRTC “order of m-lines doesn't match” bilan rad etib, Alice Bob’ning audiosini olmadi. **Tuzatildi:** server javobsiz offer’ni aynan o‘zgarmagan holda qayta yuboradi (ICE restart ham javobdan keyin); SDK subscriber offer’ni qo‘llay olmasa, subscriber’ni o‘zi qayta quradi (`subscriber.reset`). Regressiya testlari: `server/tests/signaling.rs::unanswered_offer_is_resent_unchanged` va `apps/meet/integration_test/resume_test.dart` (UI 20 s muzlatiladi, keyin resume va bot audiosi o‘lchanadi: 4 s da 16 KB audio keldi, m-line xatosi yo‘q). Linux serverda (Wi‑Fi orqali) qayta tekshirilganda yana bir xato chiqdi: muzlash paytida socket uzilib, javob yuborilmagan; resume’dan keyin klient javobni yangi socket’da `r2` id bilan yubordi, server esa uni eski sessiyadagi `r2` javobi bilan keshdan qaytarib, answer’ni qo‘llamadi (SDK har socket’da id’ni `r1`dan boshlar edi). **Tuzatildi:** SDK id’lariga har socket uchun tasodifiy prefiks qo‘shildi, server keshi `id` bilan birga so‘rov mazmunini ham tekshiradi (`reused_request_id_after_resume_runs_the_new_request`). Linux serverda natija: resume’dan keyin 4 s da 16 640 B audio.
* Debug build har safar qayta yig‘ilganda macOS kamera ruxsatini yangi binary uchun qayta so‘raydi yoki rad etadi. Imzolangan release build’da bu muammo yo‘q.

## Yuklama sinovi (25 ishtirokchi)

```bash
cargo build --release --bins
./target/release/ecochain-rtc-server serve --config config/development.toml
./target/release/loadtest --participants 25 --max-video 8 --seconds 20
```

Har bot: Opus o‘lchamidagi audio (50 kadr/s) + VP8 o‘lchamidagi kamera, 3 simulcast qatlam (150/500/1500 kbit/s), barcha audiolarga va `--max-video` ta videoga “high” sifatda obuna. Generator va server bitta Mac’da (generator ~1,5–1,7 yadro oladi).

| Konfiguratsiya | Audio yetkazildi | Video yetkazildi (o‘rt./min) | Qabul / qatnashchi | Server CPU |
| --- | --- | --- | --- | --- |
| 10 kishi × 8 video | 100% | 100% / 96% | ~10 Mbit/s | ~33% |
| 25 × 4 video | 100% | 95,5% / 90% | ~5 Mbit/s | — |
| 25 × 6 video | 100% | 99,7% / 99% | ~10 Mbit/s | — |
| 25 × 8 video (3 marta) | 98–100% | 71–77% / 47–66% | 6,5–9,3 Mbit/s | ~110–125% |

Xulosa:

* **25 kishilik xona audio bo‘yicha tasdiqlandi** (barcha 24 manba har qabul qiluvchida ≥98%).
* Video uchun 25 kishi × 6 tagacha HQ video har qurilmada — barqaror. 25 × 8 HQ (jami ~225 Mbit/s egress) — bitta xona aktori bitta yadroga yetadi va video kadrlarning ~25% yo‘qoladi. Real ilovada galereya tile’lari `low/medium` qatlam so‘raydi va mobil standart limit 4 ta video, bu konfiguratsiya yuqoridagi barqaror qatorlarga to‘g‘ri keladi.
* Yuklama paytida topilib tuzatilgan muammolar: `sendto` xona taskining ~40% vaqtini olardi (alohida yuborish thread’i, o‘tkazuvchanlik ×2); `ping` xona taski orqali o‘tib, band xonada sog‘ klientlarni sekinlashtirardi (endi signaling taskida javob beriladi).
* Bu raqamlar loopback’da, generator bilan bir mashinada olingan. Ikki qurilmali prototip natijasi bilan 25 kishilik sig‘im **alohida**: 25 kishilik sig‘im faqat headless botlar bilan o‘lchandi, 25 ta haqiqiy qurilma bilan sinov o‘tkazilmagan.

## 100 kishilik xona (2026-11-20, `feat/100-participant-rooms`)

**Sharoit (muhim):** Apple Silicon Mac, 10 yadro, release build, **loopback**, generator (`loadtest`) va server bitta mashinada (generator ham CPU oladi, shuning uchun raqamlar serverga nisbatan pessimistik, lekin oraliq jim emas: run’lar orasida chayqalish bor). Spec bo‘yicha hal qiluvchi o‘lchov — 16 yadroli Linux server + LAN orqali alohida generator; u **hali bajarilmagan** (pastdagi “Hali bajarilmagan” bo‘limi).

```bash
cd server && cargo build --release --bins && ulimit -n 65536
./target/release/ecochain-rtc-server serve --config config/development.toml &
curl -X POST http://127.0.0.1:7880/v1/rooms -H 'Authorization: Bearer dev-only-admin-key' \
  -H 'content-type: application/json' -d '{"room_id":"loadtest","max_participants":100}'
./target/release/loadtest --config config/development.toml --participants 100 --publishers 2 \
  --cameras 10 --muted-mics 30 --max-video 4 --high-video 1 --switch-every-secs 5 --seconds 30 \
  --metrics-url http://127.0.0.1:7880/metrics        # + --unmute-probe: birinchi unmute kechikishi
```

Ssenariy: 100 ishtirokchi; 2 gapiruvchi (audio + 3 qatlamli video), 10 faqat kamera, 30 muted mikrofon (muzokara qilingan), 58 media’siz; har kishi 4 video, shundan 1 tasi `high`, qolganlari `low`; har 5 s da tinglovchilar bitta video obunani almashtiradi.

### Yakuniy natijalar (100 kishi, 30 s)

| Run | Audio | Video o‘rt./min | Switch p50 / p95 | Room busy |
| --- | --- | --- | --- | --- |
| 1 | 100% | 99,9% / 97,7% | 186 / **655 ms** | **76,6%** |
| 2 | 100% | 100% / 99,8% | 78 / 333 ms | 58,3% |
| 3 | 100% | 100% / 99,8% | 77 / 342 ms | 59,0% |
| 4 (qayta) | 100% | 100% / 99,9% | 79 / 111 ms | 59,0% |
| 5 (`--unmute-probe`) | 100% (*) | 100% / 99,8% | 88 / 335 ms | 60,4% |

(*) probe qo‘shimcha audio keltirgani uchun “kutilgan”dan oshadi (avg 146,9%). Birinchi unmute eshitilishi (n=99 tinglovchi): **p50 190 ms, p95 356 ms** (faqat o‘lchandi; spec bo‘yicha mezon emas, baseline p95 288 ms edi).

Run 1 ajralib turadi (p95 655 ms, busy 76,6%) — mashinadagi tashqi yuklama yoki generator bilan raqobat bo‘lishi mumkin, lekin isbotlanmagan. Uni olib tashlamadik.

### Har vazifadan keyingi dinamika (xuddi shu ssenariy, Mac)

| Bosqich | Room busy | Switch p95 |
| --- | --- | --- |
| Baseline (o‘zgarishlardan oldin) | 52,5% | 68 ms; birinchi unmute p95 288 ms |
| + datagram marshrut keshi | 46,7% | — |
| + trek → subscriber indeksi | 46,3–47,6% | — |
| + audio tanlovchi (loudest-3) | ~48% (`--unmute-probe` bilan 54–55%; xuddi shu probe run’i tanlovchisiz oldingi commit’da 58,6%) | 76–87 ms |
| + keyframe birlashtirish (yakuniy kod, Task 6) | 49,8–53,2% | 102 / 330 / 335 ms (p50 ~66–77) |
| Bugungi yakuniy run’lar (yuqorida) | 58–60% (bitta 76,6%) | 111–342 ms (bitta 655 ms) |

Bugungi busy Task 6 dagidan ~7–9 punkt baland: kod o‘zgarmagan, demak bu mashina yuklamasi/ketma-ket run’lar farqi (sababi o‘rganilmadi). Baseline bilan solishtirish uchun Task 1 dan beri har run bir xil mashinada, lekin kunning boshqa vaqtida olingan — shuning uchun mutlaq son emas, tartib muhim.

**Switch p95 savdosi:** baseline ~70 ms dan ~330 ms gacha o‘sdi. Sabab — keyframe so‘rovlarini birlashtirish: bitta kameraga bir necha tinglovchi bir oynada o‘tsa, keyingilarining PLI’si oyna oxirigacha kechiktiriladi (shoshilinch 250 ms, oddiy 500 ms). Evaz — bitta publisher’ga keyframe yog‘ilishi yo‘q (spec bo‘yicha asosiy maqsad) va kichikroq bandwidth/CPU. Spec ~150 ms shoshilinch oyna deb yozgan; **250 ms tanlandi**. Xavfsizlik ko‘rigi bitta qatlamga ≤4 shoshilinch so‘rov/s talab qildi (keyframe kuchaytirishini cheklash uchun). Qiymat o‘lchov bo‘yicha tanlandi: 150 ms oyna + “500 ms da bir marta” cheklovi bilan bitta run’da p95 564 ms chiqdi (maqsaddan yuqori). 250 ms bilan p95 ≤500 ms maqsadiga yakuniy kod run’larida tushdi: yuqoridagi jadvalning “keyframe birlashtirish” qatoridagi 3 run (102 / 330 / 335 ms) va bugungi 5 run’dan 4 tasi.

### 25 kishilik mavjud ssenariylar (`--participants 25 --seconds 20`)

| Konfiguratsiya | Audio (**) | Video o‘rt./min (avval) | Qabul / qatnashchi | Room busy |
| --- | --- | --- | --- | --- |
| 25 × 4 video | 96,0% (min 66,7%) | 100,1% / 100% (95,5% / 90%) | ~6,2 Mbit/s | 28,4% |
| 25 × 6 video | 96,0% (min 66,7%) | 100,1% / 100% (99,7% / 99%) | ~9,2 Mbit/s | 32,6% |
| 25 × 8 video | 96,0% (min 66,7%) | 100,1% / 100% (71–77% / 47–66%) | ~12,0 Mbit/s | 42,0% |

(**) Bu **regressiya emas, hisoblash artefakti**: 25 ta teng baland gapiruvchida server faqat 3 ta audio uzatadi (`max_forwarded_audio`), loadtest esa “kutilgan” = 3 deb sanaydi; tanlangan 3 gapiruvchining o‘zi o‘z audiosini olmaydi (2/3 = 66,7%), (22·100 + 3·66,7)/25 = 96,0% aynan mos keladi. Demak 25 kishilik xonada eshitiladigan audio endi dizayn bo‘yicha 3 ta (avval 24 ta); bu spec’ning maqsadi. Video esa aniq yaxshilandi (25 × 8: ~25% yo‘qotish → 0%), busy avvalgi ~110–125% CPU dan 42% ga tushdi (busy = xona tasking band vaqti, server jami CPU emas; sendto thread’i alohida). Eslatma: “avval” raqamlari 2026-11-18 dagi jadvaldan.

### Stress: 100 kishi, 10 gapiruvchi (audio cheklovi)

`--publishers 10` (qolgani xuddi shunday): audio avg 99,0% (min 66,7%) — 10 gapiruvchidan faqat 3 tasi uzatiladi, har tinglovchi 3 ta oqim oladi (o‘sha 3 gapiruvchi o‘zi 2 ta), (97·100+3·66,7)/100 = 99,0%. Video 100,0% / min 99,7%, switch p50 111 ms / p95 354 ms, room busy 74,3% (2 gapiruvchiga nisbatan ~15 punkt ko‘p: 8 qo‘shimcha gapiruvchining video qatlamlari kirishi). Cheklov ishlaydi: 10 ta kirish audio’dan faqat 3 tasi chiqadi.

### Muvaffaqiyat mezonlari bilan solishtirish

Hal qiluvchi sharoit (Linux 16 yadro, LAN) hali o‘lchanmagan, shuning uchun Mac natijasi “⏳” bilan belgilandi, agar u o‘tsa ham.

| Mezon | Mac loopback natijasi | Holat |
| --- | --- | --- |
| Audio ≥ 99% | 100% (barcha run’lar) | ✅ Mac / ⏳ Linux |
| Video o‘rt. ≥ 98%, min ≥ 95% | o‘rt. 99,9–100%, min 97,7–99,9% | ✅ Mac / ⏳ Linux |
| Xona aktori CPU ≤ 70% (bitta yadro) | 58,3–60,4% (4 run), **76,6% (1 run)** | ⏳ (Mac: 5 dan 4 tasi o‘tdi; bitta run 70% dan oshdi — ochiq yoziladi) |
| Switch p95 ≤ 500 ms | 111–342 ms (4 run), **655 ms (1 run)** | ⏳ (Mac: 5 dan 4 tasi o‘tdi; run 1 maqsaddan oshdi) |
| Yangi gapiruvchi — birinchi audio paket kesilmaydi | `only_loudest_three_audio_tracks_are_forwarded`, `leaving_speaker_frees_audio_slot`, `loud_speaker_is_announced` testlari o‘tadi (trek muzokara qilingan, renegotiation yo‘q) | ✅ (testlar; haqiqiy qurilmada tinglab ko‘rilmagan ⏳) |
| Birinchi unmute (faqat yoziladi) | p50 190 ms, p95 356 ms (Mac, loopback; haqiqiy qurilmada renegotiation sekinroq bo‘lishi mumkin) | ✅ yozildi |
| 25 kishida regressiya yo‘q | video yaxshilandi, busy past; audio 3 ga cheklangan (dizayn) | ✅ (audio artefakti yuqorida tushuntirilgan) |

**Bajarilmagan/shubhali mezonlar:** (1) Linux’dagi hal qiluvchi o‘lchov yo‘q; (2) Mac’da run-to-run chayqalish katta (busy 49–77%, switch p95 100–655 ms), shuning uchun CPU va switch mezonlarini “✅” deb bo‘lmaydi; (3) switch p95 baseline’dan ~5 baravar o‘sdi (yuqoridagi savdo). Agar Linux’da CPU ≤70% bajarilmasa, spec bo‘yicha loyiha natijani hujjatlab tugaydi va keyingi qadam sifatida 6-qism (subscriber egress’ni bir necha thread’ga taqsimlash) yoki video slotlar alohida dizaynda taklif etiladi; bu reja doirasida amalga oshirilmaydi.

### Hali bajarilmagan

Bu qadamlar bu mashinadan (Linux server va haqiqiy qurilmalarga ulanish yo‘q) bajarib bo‘lmadi. Foydalanuvchi bajarishi kerak; raqamlar o‘ylab topilmagan.

Faqat LAN, `development.toml` dev kalitlari bilan — internetga ochmang.

1. **16 yadroli Linux server, generator boshqa mashinada (LAN, simli yoki router):**
   ```bash
   # Linux serverda
   cd server && cargo build --release --bins && ulimit -n 65536
   ./target/release/ecochain-rtc-server serve --config config/development.toml &
   curl -X POST http://<SERVER_IP>:7880/v1/rooms -H 'Authorization: Bearer dev-only-admin-key' \
     -H 'content-type: application/json' -d '{"room_id":"loadtest","max_participants":100}'
   # Generator mashinasida (--bind-ip — generatorning LAN IP’si)
   ./target/release/loadtest --config config/development.toml --url ws://<SERVER_IP>:7880/v1/rtc \
     --bind-ip <GENERATOR_LAN_IP> --participants 100 --publishers 2 --cameras 10 --muted-mics 30 \
     --max-video 4 --high-video 1 --switch-every-secs 5 --seconds 30 \
     --metrics-url http://<SERVER_IP>:7880/metrics
   # 3 marta; keyin --unmute-probe bilan 1 marta; so‘ng 25 kishilik ssenariylar:
   ./target/release/loadtest --config config/development.toml --url ws://<SERVER_IP>:7880/v1/rtc \
     --bind-ip <GENERATOR_LAN_IP> --participants 25 --max-video 4 --seconds 20   # 6 va 8 uchun ham
   ```
   Natijani mezonlar jadvaliga kiriting (⏳ → ✅/❌). Generator 100 botda ancha CPU oladi — alohida mashinada ham yadrolar yetarliligini tekshiring.
2. **Qo‘lda Meet testi (2 haqiqiy qurilma + 50 bot):** `loadtest --participants 50 --publishers 2 --max-video 4` ni xonada ishga tushiring, 2 qurilmada Meet ilovasi bilan o‘sha xonaga kiring. Tekshiriladi: gapiruvchi almashganda ovoz uzilmasligi (birinchi bo‘g‘in kesilmasligi), boshqa videoga o‘tish tezligi (qora ekran bir necha soniya turmasligi), birinchi marta mikrofon yoqishdagi kechikish. Natija shu yerga yoziladi.

## Platformalar

| Platforma | Build | Haqiqiy sinov | Izoh |
| --- | --- | --- | --- |
| macOS | ✅ `flutter build macos` | ✅ kamera + mikrofon, publish va qabul | Ekran ulashish kodi bor, qo‘lda sinalmagan |
| iOS Simulator | ✅ | ✅ mikrofon, video qabul, chat ↔ meet, reconnect, leave | Simulatorda kamera yo‘q |
| iOS qurilma | ❌ sinalmagan | ❌ | Imzo (team) talab qilinadi; ekran ulashish uchun Broadcast Extension hali yo‘q |
| Android | ❌ build qilinmagan | ❌ | Manifest ruxsatlari qo‘shilgan; ekran ulashish uchun MediaProjection foreground service ilova tomonidan qo‘shilishi kerak |
| Windows | ❌ build qilinmagan (Mac’da imkonsiz) | ❌ | |
