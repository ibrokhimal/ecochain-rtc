# Sinov natijalari

Sana: 2026-11-18. Mashina: Apple Silicon Mac (10 yadro), macOS 26, Rust 1.96, Flutter 3.44.1 / Dart 3.12.1.

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

Tarmoq almashganda: media socket IP’si faqat ishga tushishda aniqlanadi, shuning uchun server yangi Wi‑Fi’da **qayta ishga tushirilishi kerak** (aks holda signaling ishlaydi, lekin media eski IP’ga e’lon qilinadi). Yangi IP: serverda `hostname -I`.

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

## Platformalar

| Platforma | Build | Haqiqiy sinov | Izoh |
| --- | --- | --- | --- |
| macOS | ✅ `flutter build macos` | ✅ kamera + mikrofon, publish va qabul | Ekran ulashish kodi bor, qo‘lda sinalmagan |
| iOS Simulator | ✅ | ✅ mikrofon, video qabul, chat ↔ meet, reconnect, leave | Simulatorda kamera yo‘q |
| iOS qurilma | ❌ sinalmagan | ❌ | Imzo (team) talab qilinadi; ekran ulashish uchun Broadcast Extension hali yo‘q |
| Android | ❌ build qilinmagan | ❌ | Manifest ruxsatlari qo‘shilgan; ekran ulashish uchun MediaProjection foreground service ilova tomonidan qo‘shilishi kerak |
| Windows | ❌ build qilinmagan (Mac’da imkonsiz) | ❌ | |
