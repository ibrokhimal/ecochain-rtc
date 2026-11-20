# 100+ kishilik xonalar — dizayn

Sana: 2026-11-20. Holat: muhokamada kelishilgan, tasdiq kutilmoqda.

## Maqsad va sabab

Hozir bitta xona 25 kishi bilan sinalgan va bitta CPU yadrosiga tayanadi
(`docs/TESTING.md`, 25 × 8 HQ video’da kadrlarning ~25% yo‘qoladi). Mahsulot
Zoom’dagidek 100+ kishilik uchrashuvlarni talab qiladi. Ustuvorlik: **tezlik
(past kechikish, tez ochilish) va sifat**.

Ishlatilish holati (foydalanuvchi aytgan):

* Bir vaqtda **1–2 kishi gapiradi**, qolganlar tinglaydi.
* Ko‘pincha **faqat gapiruvchining videosi** yoqiq; ba’zi tinglovchilar kamerasi yoqiq bo‘lishi mumkin.
* Foydalanuvchi Zoom’dagidek **istalgan ishtirokchiga o‘tib ko‘ra olishi** kerak (mahkamlash / tile tanlash).

## Muvaffaqiyat mezonlari

`loadtest` bilan, server va generator alohida mashinalarda yoki loopback’da
(natija qaysi sharoitda olingani yoziladi):

| Ssenariy | Mezon |
|---|---|
| 100 kishi: 2 gapiruvchi (audio + 3 qatlamli video), 10 tinglovchi kamerasi yoqiq, 88 tinglovchi media’siz; har kishi 4 video obuna | audio ≥ 99%, obuna bo‘lingan video ≥ 98% (o‘rtacha), min ≥ 95% |
| Xuddi shu, xona aktori CPU’si | bitta yadroning ≤ 70% (zaxira qoladi) |
| Boshqa ishtirokchi videosiga o‘tish (obuna → birinchi kadr) | p95 ≤ 500 ms |
| Yangi gapiruvchi (3 dan kam faol gapiruvchi bor, audio trek allaqachon muzokara qilingan) | birinchi audio paketi kesilmaydi |
| Sessiyada mikrofonni **birinchi** yoqish | qabul qilinadi: renegotiation davomidagi kechikish (~0,3–1 s) — foydalanuvchi qarori (a); faqat o‘lchanadi va hujjatlanadi |
| 25 kishilik mavjud ssenariylar | regressiya yo‘q (TESTING.md jadvali) |

O‘lchov shartlari (quruvchi qarori sifatida belgilandi):

* **Hal qiluvchi natija** — 16 yadroli Linux server (TESTING.md’dagi), generator boshqa mashinada, simli yoki router LAN. Mac loopback faqat ishlab chiqish paytidagi tez tekshiruv.
* **Video obunalari** real SDK siyosatiga mos (`subscription_policy.dart`): faol gapiruvchi / mahkamlangan — `high`, qolgan ko‘rinib turgan tile’lar — `low`. Hammasini `high` so‘rash — alohida stress ssenariy, mezon emas.
* **O‘tish vaqti** foydalanuvchi ko‘radigan vaqt: obuna so‘rovidan birinchi kadrgacha, SDP renegotiation ham ichida.
* **Mikrofonli tinglovchilar** ham sinaladi: 100 kishidan 30 tasi mikrofonni bir marta yoqib o‘chirgan (muzokara qilingan, muted audio trek). Join va renegotiation kechikishi alohida metrika.
* Agar 2–5-qismlardan keyin CPU mezoni bajarilmasa, bu loyiha natijani hujjatlab tugaydi va 6-qism alohida dizaynga o‘tadi.

## Maqsad emas (hozir)

* Serverlararo kaskad, webinar rejimi (500+ tomoshabin).
* Doimiy audio slotlar (B yo‘l), serverda audio aralashtirish (C yo‘l).
* SVC kodeklar / RTP passthrough rejimi.
* Serverning o‘zida ICE-TCP (TCP fallback TURN orqali hal qilinadi).
* Flutter SDK’ni o‘zgartirish (ommaviy API ham, ichki qism ham). Audio sender’ni oldindan yaratish (b/c variantlar) rad etildi.

## Anti-maqsadlar (texnik ishlasa ham muvaffaqiyatsizlik)

* Gap boshlanishi (birinchi bo‘g‘in) kesilishi — muzokara qilingan trekda; birinchi yoqishdagi kechikish ongli ravishda qabul qilingan.
* Gapiruvchilar tez-tez almashinib, ovoz “uzuq-yuluq” bo‘lishi.
* Boshqa odamga o‘tganda video bir necha soniya qora turishi.
* 25 kishilik xonalarda sifat yoki kechikish yomonlashishi.

## Cheklovlar

* Rust server, `str0m` 0.25, “frame mode” (o‘zgarmaydi).
* Xona = bitta tokio aktor; bu model saqlanadi.
* Mavjud protokol (`docs/PROTOCOL.md`) orqaga mos qolishi kerak; yangi maydonlar ixtiyoriy.
* SDK’da mikrofon o‘chirilganda sender saqlanadi (`replaceTrack(null)` + `mute`), shuning uchun audio m-line har ishtirokchi uchun sessiyada bir marta qo‘shiladi — A yo‘l shu xatti-harakatga tayanadi.

## Yo‘llar (audio uzatish)

| Yo‘l | Mohiyati | Afzallik | Kamchilik |
|---|---|---|---|
| **A (tanlandi)** | Har audio trek o‘z m-line’ida qoladi; server faqat eng baland N (standart 3) gapiruvchi paketlarini yozadi, qolganini tashlaydi | SDK/UI o‘zgarmaydi; ish va xavf kam; 1–2 gapiruvchi holatiga to‘liq yetadi | Mikrofon birinchi marta yoqilganda hamma subscriber’ga renegotiation; 300+ kishida SDP katta |
| B | Har qurilmada doimiy 3 audio slot, server manbani almashtiradi | SDP doim kichik, renegotiation yo‘q | SDK/UI o‘zgaradi, slot almashishida vaqt qayta yozish va Opus uzilishi xavfi |
| C | Serverda audio mixing | Klientda bitta oqim | CPU qimmat, kechikish qo‘shadi, alohida ovoz boshqaruvi yo‘q |

Foydalanuvchi **A**ni tanladi: hozirgi holatga yetarli, tez chiqadi, B’ga keyin o‘tishni to‘smaydi.

## Dizayn

Qismlar bajarilish tartibida. Har qadamdan keyin `loadtest` bilan o‘lchanadi va
natija `docs/TESTING.md`ga yoziladi.

### 1. Yuklama testini kengaytirish (asosiy chiziq)

`server/src/bin/loadtest.rs` va `bot.rs`:

* `--publishers N` — audio + 3 qatlamli video yuboradigan botlar (gapiruvchilar, audio darajasi baland, masalan −25 dBov).
* `--cameras M` — faqat kamera yuboradigan tinglovchilar.
* Qolganlar media yubormaydi, faqat qabul qiladi.
* Kutilgan audio = gapiruvchilar soni (N ≤ 3 bo‘lsa), video = `min(max_video, video manbalari)`.
* `--switch` rejimi: har bot vaqti-vaqti bilan obunasini boshqa trekka o‘tkazadi va “obuna → birinchi kadr” vaqtini o‘lchaydi (p50/p95).
* Server CPU’si (xona aktori thread’i) chiqishda ko‘rsatiladi; mavjud `/metrics` yetmasa, metrika qo‘shiladi.

Avval o‘zgarishsiz serverda 50 va 100 kishilik asosiy chiziq olinadi.

### 2. O(N) qidiruvlarni olib tashlash

* `Room::handle_datagram` har paketda barcha peer’larni `accepts` bilan aylanadi. Manba manzili → (ishtirokchi id, peer turi) xaritasi qo‘shiladi; xaritada yo‘q yoki noto‘g‘ri bo‘lsa, eski aylanish zaxira sifatida ishlaydi va natija keshga yoziladi. Peer yopilganda / ishtirokchi chiqqanda yozuv o‘chadi.
* `Room::forward` har paketda barcha ishtirokchilarni va har birining `sub.out` ro‘yxatini aylanadi. Trek id → shu trekni `Open` holatda olayotgan subscriber’lar ro‘yxati indeksi qo‘shiladi; `reconcile` / `subscriber_answer` / chiqishda yangilanadi.
* `drive` har raundda barcha peer’larni `is_due` bilan tekshiradi — bu arzon, lekin o‘lchovda ko‘rinsa, “uyg‘otilgan peer’lar” to‘plami bilan almashtiriladi.

Indekslarni qanday saqlash (ishtirokchi indeksi siljishi, id bo‘yicha xarita) — quruvchi qarori; talab: xato indeks hech qachon boshqa ishtirokchiga paket yubormasin (fallback va testlar bilan).

### 3. Eng baland N audio (A yo‘l)

Yangi tanlovchi (masalan `room/audio.rs`, sof funksiyalar, unit test qilinadigan):

* Har audio trek uchun silliqlangan daraja: har paketning RFC 6464 `audio_level`idan, tez ko‘tarilish (~20 ms) va sekin tushish (~300 ms).
* **Uzatiladigan to‘plam** (≤ `rooms.max_forwarded_audio`, standart 3):
  * Ostonadan baland trek, to‘plamda bo‘sh joy bo‘lsa, **darhol** kiradi (o‘sha paketning o‘zi uzatiladi — birinchi bo‘g‘in kesilmaydi).
  * To‘plam to‘la bo‘lsa, yangi trek eng past a’zodan aniq farq (masalan +6 dB) bilan baland bo‘lgandagina uni almashtiradi.
  * A’zo ostonadan pastga tushgach, yana ~500 ms ushlab turiladi (gap orasidagi pauzalar uzilmasin).
* `muted` trek paketlari hech kimga yozilmaydi.
* To‘plamdan tashqari paketlar `writer.write`gacha yetmaydi (CPU tejaladi).
* `audio_level` kengaytmasi yo‘q trek (eski klient) doim uzatiladi — moslik uchun.
* Qabul qiluvchi tomonda str0m o‘z sequence raqamlarini beradi, shuning uchun uzilish DTX kabi ko‘rinadi; buni haqiqiy klient bilan (Meet ilovasi) eshitib tekshirish rejaga kiradi.

### 4. Faol gapiruvchi hodisasini tezlashtirish

* `update_speakers` (500 ms oyna, `TICK` bilan) 3-qismdagi holatdan foydalanadi; `speakers.changed` tarkib o‘zgarganda yuboriladi, lekin 250 ms’dan tez emas.
* Hodisa formati o‘zgarmaydi.

### 5. Videoga tez o‘tish (keyframe)

Hozirgi muammolar (`request_keyframe`):

* Throttle (500 ms) trek bo‘yicha, rid’ni hisobga olmaydi — boshqa qatlam uchun so‘rov yutilib ketishi mumkin.
* Throttle ichida kelgan so‘rov **tashlanadi**: yangi obunachi keyingi tabiiy keyframe’gacha (bir necha soniya) qora ekran ko‘rishi mumkin.
* `forward` ichida `wanted_keyframe` har paket va har subscriber uchun qo‘shiladi.

Dizayn:

* Throttle (trek, rid) bo‘yicha.
* Throttle davomida kelgan so‘rov “kutilmoqda” deb belgilanadi va throttle tugashi bilan bitta so‘rov yuboriladi (birlashtirish). 98 kishi birdan obuna bo‘lsa ham publisher 500 ms’da ≤ 1 keyframe/qatlam oladi.
* Xona aktori throttle tugash vaqtini `next_timeout`ga qo‘shadi.
* Yangi ochilgan obuna (m-line endigina `Open` bo‘lgan) uchun so‘rov qisqaroq throttle bilan (taxminan 150 ms) o‘tadi, aks holda renegotiation + 500 ms kutish o‘tish byudjetini yeb qo‘yadi. Qiymat o‘lchab tanlanadi.
* Mezon bajarilmasa, keyingi qadam qayta ishlatiladigan video m-line’lar (“video slotlar”) — bu loyihada emas, natija bilan birga taklif qilinadi.

### 6. Egress’ni parallellashtirish (shartli)

Faqat 2–5-qismlardan keyin 100 kishilik ssenariyda xona aktori yadroning > 70%ini olsa. Unda subscriber peer’larining `Rtc` holatini (SRTP + paketlash) bir nechta worker task’ga bo‘lish varianti alohida dizayn va reja bilan ko‘riladi. Hozirgi hujjat uni amalga oshirmaydi.

### 7. TURN (UDP bloklangan tarmoqlar)

Server kodi allaqachon `media.ice_servers`ni join javobida klientga beradi. Qo‘shiladi:

* coturn uchun namuna konfiguratsiya (`turns:` TLS 443, `turn:` TCP/UDP 3478) va `docs/`da o‘rnatish bo‘limi.
* **Vaqtinchalik TURN ma’lumotlari** (coturn `use-auth-secret`, TURN REST API usuli): server har join uchun `username = <expiry>:<identity>`, `credential = base64(HMAC-SHA1(secret, username))` hosil qiladi. Doimiy parol konfigda klientlarga tarqalmaydi. Konfigda yangi ixtiyoriy `turn_secret`; mavjud statik `username/credential` ham ishlashda davom etadi.
* SDK o‘zgarmaydi (u `ice_servers`ni allaqachon qo‘llaydi) — tekshiriladi.

### 8. Konfiguratsiya va hujjatlar

* `rooms.max_participants_limit` standart 100 → 150 (zaxira bilan); `default_max_participants` 25 qoladi — 100 kishilik xona admin API orqali `max_participants` bilan yaratiladi.
* Yangi `rooms.max_forwarded_audio` (standart 3, 1..=8).
* `docs/ARCHITECTURE.md` (Kengaytirish, audio), `docs/PROTOCOL.md` (agar maydon qo‘shilsa), `docs/TESTING.md` (yangi natijalar).

## Testlar

* Unit: audio tanlovchi (darhol kirish, almashtirish farqi, ushlab turish, muted, kengaytmasiz trek); keyframe birlashtirish; manzil/trek indekslari.
* Integratsiya (`server/tests/media.rs`): 4 gapiruvchi → qabul qiluvchi faqat eng baland 3 tasini oladi; obunadan keyin birinchi video kadr vaqti chegarada.
* Yuklama: 1-qismdagi ssenariylar, natijalar TESTING.md’da.
* Qo‘lda: Meet ilovasida 2 haqiqiy qurilma + botlar bilan ovoz uzilishi va o‘tish tezligi.

## Quruvchiga qoldirilgan

* Indekslarning aniq ma’lumot tuzilmasi va yangilanish nuqtalari (2-qism talablari doirasida).
* Silliqlash konstantalari, ostona va hysteresis qiymatlari — o‘lchab tanlanadi, konstantalar sifatida hujjatlanadi.
* Xona CPU’sini o‘lchash usuli (metrika yoki tashqi profil).
