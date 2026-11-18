# Ishlatilgan kutubxonalar va litsenziyalar

Ecochain kodi — xususiy. Quyidagi uchinchi tomon komponentlari ruxsat beruvchi (permissive) litsenziyalarga ega; copyleft majburiyati yo‘q. Tarqatishda (ilova yoki server binari) mualliflik va litsenziya matnlari ilova qilinishi kerak.

## Rust server (`cargo metadata`, 236 paket)

| Litsenziya | Paketlar soni | Muhimlari |
| --- | --- | --- |
| MIT OR Apache-2.0 (va variantlari) | ~180 | `str0m`, `is`, `str0m-proto`, `tokio`, `serde`, `hmac`, `sha2`, `rand`, `uuid`, `clap`, `tracing` |
| MIT | 37 | `axum`, `axum-server`, `tokio-tungstenite`, `dimpl` |
| ISC / Apache-2.0 / OpenSSL turidagi | — | `aws-lc-rs`, `aws-lc-sys` (ISC AND Apache-2.0 AND BSD-3-Clause AND MIT …), `rustls` (Apache-2.0 OR ISC OR MIT), `rustls-webpki` (ISC), `ring` (Apache-2.0 AND ISC) |
| BSD-3-Clause | 1 | `subtle` |
| Unicode-3.0 (bilan) | 1 | `unicode-ident` |
| r-efi (MIT OR Apache-2.0 OR LGPL-2.1+) | 1 | MIT varianti tanlanadi |

To‘liq ro‘yxatni yangilash: `cargo metadata --format-version 1` yoki `cargo about generate` (tavsiya: release build’ga `THIRD_PARTY_NOTICES` faylini avtomatik qo‘shish).

**Bildirishnoma:** `aws-lc` (AWS-LC) — OpenSSL/BoringSSL’dan meros kod; ISC, Apache-2.0 va OpenSSL/SSLeay atributsiya matnlarini binari bilan birga yetkazing.

## Flutter

| Paket | Versiya | Litsenziya |
| --- | --- | --- |
| `flutter_webrtc` | 1.6.3 | MIT |
| `dart_webrtc` | 1.8.2 | MIT |
| `webrtc_interface` | 1.5.1 | MIT |
| `web_socket_channel` | 3.0.3 | BSD-3-Clause |
| Native WebRTC (`WebRTC-SDK` / `io.github.webrtc-sdk:android` 150.7871.04, libwebrtc asosida) | — | BSD-3-Clause + qo‘shimcha patent grant (WebRTC), ichidagi uchinchi tomon kodlari (Opus — BSD, libvpx — BSD, BoringSSL — OpenSSL/ISC, va h.k.) |

Flutter ilovalarda `LicenseRegistry` orqali Dart paketlari litsenziyalari avtomatik `showLicensePage` ga tushadi. Native libwebrtc bildirishnomalarini (`LICENSE`, `PATENTS`, `third_party` litsenziyalari) ilova “Litsenziyalar” sahifasiga yoki tarqatish paketiga qo‘shish kerak.
