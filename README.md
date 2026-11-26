# Ecochain RTC

O‘zimizning Rust media serverimiz (signaling + SFU) va Flutter SDK. LiveKit o‘rnini bosadi.

```text
server/                     Rust: signaling, xonalar, str0m SFU, yuklama generatori
packages/ecochain_rtc/      Flutter SDK (ekranlarsiz)
packages/ecochain_meeting_ui/  Umumiy Meet ekrani (SDK ustida)
apps/meet/                  Alohida Ecochain Meet ilovasi
apps/messenger_demo/        Meet ichiga joylangan Messenger demo
docs/                       Reja, arxitektura, protokol, sinovlar, litsenziyalar
```

Hujjatlar: [PLAN](docs/PLAN.md) · [ARCHITECTURE](docs/ARCHITECTURE.md) · [PROTOCOL](docs/PROTOCOL.md) · [TESTING](docs/TESTING.md) · [LICENSES](docs/LICENSES.md) · [SDK README](packages/ecochain_rtc/README.md)

> Mavjud `../meet_demo` (LiveKit asosidagi prototip) o‘zgartirilmadi; yangi tizim shu papkada.

## Talablar

Rust ≥ 1.89, Flutter ≥ 3.38 (sinovda 3.44.1), macOS uchun Xcode. UDP 7882 va TCP 7880 portlari ochiq bo‘lsin.

## 1. Serverni ishga tushirish

```bash
cd server
cargo run --release -- serve --config config/development.toml
```

* Signaling: `ws://<mac-ip>:7880/v1/rtc` (development). Production: `config/production.example.toml`, TLS majburiy.
* Media: UDP `7882`, LAN manzili va `127.0.0.1` avtomatik e’lon qilinadi (`media.listen` bilan o‘zgartiriladi).
* Kuzatuv: `curl localhost:7880/metrics` (faol xonalar, ishtirokchilar, ulanishlar, join xatolari kod bo‘yicha, peer xatolari, media hisoblagichlari), `curl localhost:7880/healthz`.

## 2. Development tokenlari

Uchta usul (hammasi faqat `mode = "development"`da yoki admin kaliti bilan):

```bash
# CLI (konfiguratsiyadagi secret bilan imzolaydi)
cargo run --release -- token --config config/development.toml --room demo --identity alice --name Alice --host
```

```bash
# HTTP, faqat development rejimida
curl -s -X POST localhost:7880/v1/dev/token -H 'content-type: application/json' -d '{"room":"demo","identity":"bob","name":"Bob"}'
```

```bash
# Admin API (production’da ham, admin kaliti bilan)
curl -s -X POST localhost:7880/v1/tokens -H 'authorization: Bearer dev-only-admin-key' -H 'content-type: application/json' -d '{"room":"demo","identity":"carol","role":"participant","ttl_secs":3600}'
```

Ilovalardagi “Development token olish” tugmasi `/v1/dev/token`ni chaqiradi. Production’da tokenni Ecochain backend’i `auth.token_secret` bilan HS256 JWT sifatida imzolaydi (claim’lar: [PROTOCOL.md §4](docs/PROTOCOL.md#4-token)).

Xonani oldindan `max_participants` bilan yaratish:

```bash
curl -s -X POST localhost:7880/v1/rooms -H 'authorization: Bearer dev-only-admin-key' -H 'content-type: application/json' -d '{"room_id":"board","max_participants":10}'
```

## 3. Ilovalarni ishga tushirish

Alohida Meet (macOS):

```bash
cd apps/meet && flutter run -d macos
```

Messenger demo (iOS Simulator yoki boshqa qurilma):

```bash
cd apps/messenger_demo && flutter run -d "iPhone 17 Pro"
```

Telefon yoki emulyator uchun server manzilini bering (Mac’ning LAN IP’si):

```bash
flutter run -d <device-id> --dart-define=ECOCHAIN_RTC_URL=ws://192.168.1.10:7880/v1/rtc
```

Ikkala ilovada bir xil xona (`demo`) va **turli** ID kiriting, “Qo‘shilish”ni bosing. Messenger’da ← tugmasi chatga qaytaradi, qo‘ng‘iroq davom etadi; yuqoridagi “Uchrashuvga qaytish” paneli shu sessiyani ochadi. Demo chat xabarlari **faqat lokal** (backend yo‘q, boshqa qurilmalarga yuborilmaydi).

Unattended sinov uchun development kalitlari: `--dart-define=ECOCHAIN_AUTOJOIN=true --dart-define=ECOCHAIN_IDENTITY=alice --dart-define=ECOCHAIN_CAMERA=false`.

## 4. Tekshirish

```bash
cd server && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
```

```bash
cd packages/ecochain_rtc && flutter analyze && flutter test
```

```bash
cd server && cargo run --release --bin loadtest -- --participants 25 --max-video 6
```

Natijalar: [docs/TESTING.md](docs/TESTING.md).

## Adaptiv video

Kamera uch sifatda simulcast yuboradi. Server har qabul qiluvchining TWCC
tezligiga qarab sifatni tanlaydi, audio uchun zaxira qoldiradi va bandwidth
sig‘magan videolarni vaqtincha pauza qiladi. Sifat 2 soniya barqaror zaxira
bo‘lganda qayta oshadi; pauzadan keyin video yangi keyframe bilan ochiladi.
Buning uchun yangi klient signaling versiyasi yoki maxsus server sozlamasi
kerak emas. Tafsilot: [tarmoqqa moslashish](docs/ARCHITECTURE.md#simulcast-va-tarmoqqa-moslashish).

## Holat

Bajarildi: birinchi bosqich (server, SDK, umumiy UI, ikki ilova, ikki qurilmali haqiqiy audio-video, reconnect, tozalash, simulcast, faol gapiruvchi, ekran ulashish API’si desktop uchun).

Hali bajarilmagan: yozib olish va shifrlangan fayl saqlash (keyingi bosqich, dizayn — ARCHITECTURE.md); TURN server (alohida coturn kerak); iOS ekran ulashish (Broadcast Extension); Android ekran ulashish foreground service; Android/Windows/iOS qurilmada sinov; serverlararo kaskad; parallel egress (bitta xona > 1 yadro).
