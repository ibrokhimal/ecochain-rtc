# Amalga oshirish rejasi

## Repository o‘rganilganda topilganlar

* `ecochain/` — git’siz ish papkasi, ichida mustaqil loyihalar: `license-manager/` (Rust, tonic/tokio, `thiserror`/`anyhow`, TOML/JSON konfiguratsiya, secret’larni env’da saqlash uslubi), `meet_demo/` (LiveKit asosidagi Flutter prototip: `MeetingSession` UI’dan tashqarida saqlanadi, Messenger uni chat ekranida ushlaydi), `outputs/` (Jira CSV: Meet MVP vazifalari).
* `AGENTS.md` repository darajasida yo‘q (faqat `build/` ichidagi uchinchi tomon paketlarida). Shuning uchun `license-manager` uslubi (Rust 2021, `anyhow` + `thiserror`, secret’lar env’dan, development/production ajratilishi) va `meet_demo` g‘oyasi (sessiya ekrandan mustaqil) qayta ishlatildi.
* Yangi tizim `ecochain-rtc/` papkasida, so‘ralgan tuzilma bo‘yicha. `meet_demo` tegilmadi.

## Kutubxonalar (rasmiy manbalardan tekshirildi, 2026-11-18)

| Kutubxona | Versiya | Litsenziya | Qaror |
| --- | --- | --- | --- |
| `str0m` | 0.25.0 | MIT OR Apache-2.0 | Tanlandi: sans-IO, SFU uchun mos; `aws-lc-rs` kripto |
| `flutter_webrtc` | 1.6.3 | MIT | Tanlandi: Android, iOS, macOS, Windows; libwebrtc `WebRTC-SDK 150.7871.04` |
| `axum` / `axum-server` | 0.8 / 0.7 | MIT | HTTP, WebSocket, rustls TLS |
| `web_socket_channel` | 3.0.3 | BSD-3-Clause | Dart WebSocket |

Kutubxona almashtirilmadi.

## Bosqichlar

1. ✅ **Asos (ikki qurilma):** protokol v1, token, xonalar, SFU, SDK, umumiy UI, ikki ilova; ikki ilova orasida haqiqiy audio-video; chat ↔ meet; tozalash.
2. ✅ **Ishonchlilik:** resume, grace, ICE restart, to‘liq reconnect, backoff+jitter, takroriy so‘rovlar, mezbon huquqlari; 5/15 s va tarmoq almashishi sinovlari.
3. ✅ **Sig‘im (birinchi o‘lchov):** simulcast, BWE bo‘yicha qatlam, video obuna siyosati, 25 kishilik yuklama sinovi.
4. ⏭ **Keyingi:** haqiqiy qurilmalarda (Android, iPhone, Windows) sinov va Wi‑Fi/LTE almashishi; TURN; mobil ekran ulashish; parallel egress.
5. ⏭ **Yozib olish va fayllar:** alohida bosqich (ARCHITECTURE.md oxirida dizayn).
