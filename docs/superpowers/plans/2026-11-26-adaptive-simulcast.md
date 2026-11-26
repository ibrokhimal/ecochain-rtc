# Adaptive Simulcast Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Har bir subscriber internetiga mos video sifatini tanlash, audio
uchun joy qoldirish va sig‘magan videolarni xavfsiz pauza/qayta tiklash.

**Architecture:** Flutter’ning mavjud q/h/f encoding’lari saqlanadi.
Rust SFU’da sof budjet allocator, vaqt bo‘yicha adaptation controller va
forwarding keyframe gate ajratiladi. Yangi majburiy signaling API bo‘lmaydi.

**Tech Stack:** Rust, Tokio, str0m =0.25.0; mavjud Flutter WebRTC publisher.

**Spec:** `docs/superpowers/specs/2026-11-26-adaptive-simulcast-design.md`

## Global Constraints

- Baseline commit: `98c0e9a`; yangi o‘zgarishlar undan keyin alohida commitlar.
- Kamera qatlamlari: 150/500/1500 kbit/s; single kamera 800, ekran 1500.
- Boshlang‘ich baho 800 kbit/s; noma’lum baho cheklanmagan tezlik emas.
- Video budjeti: `max(0, floor(estimate_kbps * 0.85) - audio_reserve_kbps)`.
- Har eligible audio trekka 64 kbit/s; mikrofonlar max_forwarded_audio bilan
  cheklanadi, ekran audiosi alohida hisoblanadi.
- Upgrade/resume: 2 soniya barqaror talab va kamida 20 foiz qo‘shimcha budjet.
- Pauza negotiation holatini o‘zgartirmaydi; resume keyframe bilan boshlanadi.
- Priority: ekran, high kamera, faol gapiruvchi, qolgan kameralar.
- Production uchun bandwidth override API yoki yangi dependency qo‘shilmaydi.
- Qurilma yoki real transport sinovi bajarilmasa, bajarildi deb yozilmaydi.

## Review Focus

- Nol baho va juda katta u64 qiymatlar: panic/overflow yoki unlimited fallback
  bo‘lmasin (Task 1).
- Muted/unpublished trek va speaker o‘zgarishi: audio zaxirasi hamda video
  priority eskirib qolmasin (Tasks 1, 3).
- Upgrade kutayotganda subscription tartibi o‘zgarishi: timer boshqa trekka
  ko‘chib qolmasin (Task 2).
- Bitta encoding va o‘sha RID’ga qaytish: delta frame’dan boshlanmasin (Task 3).
- Barcha video pauzada: transport probing davom etsin va subscriber mustaqil
  tiklana olsin (Task 4).

## File Map

- `server/src/sfu/layers.rs`: nominal bitrate modeli, sof allocator, Timeline.
- `server/src/sfu/adaptation.rs` (yangi): budjet hisoblash va vaqt controller’i.
- `server/src/sfu/mod.rs`: yangi modulni e’lon qilish.
- `server/src/sfu/peer.rs`: boshlang‘ich BWE limitini umumiy konstanta bilan moslash.
- `server/src/room/participant.rs`: subscriber adaptation va video gate holati.
- `server/src/room/media.rs`: audio/speaker konteksti, BWE reaction, forwarding.
- `server/src/room/adaptation_tests.rs` (yangi, cfg(test)): real SFU test harness.
- `server/src/bot.rs`: qabul qilingan qatlamni ajratishga yetadigan diagnostika.
- `server/tests/media.rs`: amaldagi media regressiyalarini yangilash.
- `docs/ARCHITECTURE.md`, `docs/TESTING.md`, `README.md`: yakuniy xatti-harakat
  va faqat amalda olingan sinov natijalari.

### Task 1: Budjetga sig‘adigan allocator

**Files:** `server/src/sfu/layers.rs`, `server/src/sfu/adaptation.rs`,
`server/src/sfu/mod.rs`, `server/src/sfu/peer.rs`.

**Interfaces:**

- `Allocation`: `Paused | Layer(usize)`; Copy, Eq.
- `VideoDemand { layers: usize, quality: Quality, screen: bool, speaker: bool }`.
- `VideoDemand::cost_kbps(&self, allocation: Allocation) -> u64`.
- `allocate(demands: &[VideoDemand], budget_kbps: u64) -> Vec<Allocation>`.
- `video_budget(estimate_kbps: Option<u64>, microphone_count: usize,
  screen_audio_count: usize, max_forwarded_audio: usize) -> u64`.
- `INITIAL_SUBSCRIBER_KBPS: u64 = 800`, peer va budjet hisobida bir xil.

- [ ] Failing tests yozish: `video_budget(None, 1, 0, 3) == 616`,
  `video_budget(Some(0), 1, 0, 3) == 0`,
  `video_budget(Some(1000), 8, 1, 3) == 594`.
- [ ] Allocator testlari: 3 ta high kamera va 300 kbit/s budjet =>
  `[Layer(0), Layer(0), Paused]`; bitta high kamera va 150/500/1500 budjet =>
  mos ravishda Layer(0)/Layer(1)/Layer(2); low talab hech qachon Layer(1) olmaydi.
- [ ] Priority va single encoding testlari: screen/high/speaker/oddiy tartibi;
  1500 kbit/s ekran sig‘masa 150 kbit/s kamera yuborilishi;
  800 kbit/s single kamera 799 da pauza bo‘lishi.
- [ ] Chegara testlari: bo‘sh ro‘yxat, 0..=3 qatlam, katta u64 budjet,
  yig‘indi xarajat har doim budjetdan oshmasligi.
- [ ] `cargo test --lib sfu::` bilan yangi testlar avval yiqilishini ko‘rish.
- [ ] Ikki bosqichli (bazaviy joy, keyin upgrade) barqaror allocator va
  overflow’siz zaxira hisoblashni yozish; Timeline xatti-harakatini saqlash.
  Interface migratsiyasi sabab media.rs call site’ini minimal moslash;
  to‘liq forwarding gate Task 3’da.
- [ ] `cargo test --lib sfu::` natijasi PASS bo‘lishi.
- [ ] Commit: `feat: allocate video within subscriber bandwidth budget`.

### Task 2: Barqaror upgrade va tez downgrade

**Files:** `server/src/sfu/adaptation.rs`, `server/src/room/participant.rs`.

**Consumes:** Task 1 `VideoDemand`, `Allocation`, `allocate`, `cost_kbps`.

**Produces:** `AdaptationState::update(&mut self,
requests: &[(&str, VideoDemand)], budget_kbps: u64, now: Instant)
-> Vec<Allocation>`. Requests’dagi str — xona ichida yagona track ID.

- [ ] Fake vaqtli failing test: amaldagi qatlamdan pastga o‘tish shu call’da;
  kerakli yangi taqsimot narxi 500 bo‘lsa 599 budjetda upgrade yo‘q,
  600 budjetda 1999 ms’da yo‘q, 2000 ms’da upgrade bor.
- [ ] Past baho yoki kerakli taqsimot o‘zgarsa timer reset bo‘lishi;
  pause’dan qaytish ham 2 soniya kutishi; subscription olib tashlanishi,
  kamaygan sifat va nol budjet darhol bajarilishini sinash.
- [ ] Ro‘yxat tartibi o‘zgarsa holat track ID bilan mos kelishini va yangi
  subscriber default holati oldingi baho/timer’ni olmasligini sinash.
- [ ] `cargo test --lib sfu::adaptation` bilan RED natijani tasdiqlash.
- [ ] `AdaptationState`ni implement qilish: current/pending track ID bo‘yicha;
  oldin zarur downgrade/pause’lar, keyin umumiy desired allocation uchun
  headroom va vaqt tekshiruvi. Aralash eski/yangi holat ham budjetdan oshmasin.
  Birinchi call’da budjetga mos boshlang‘ich taqsimot kutmasdan qo‘llanadi.
- [ ] Testlar PASS, umumiy allocation cost <= budget invariant’i ham PASS.
- [ ] Commit: `feat: stabilize subscriber video quality recovery`.

### Task 3: Room va forwarding integratsiyasi

**Files:** `server/src/room/participant.rs`, `server/src/room/media.rs`,
`server/src/room/adaptation_tests.rs`, `server/src/room/mod.rs`.

**Consumes:** Task 2 controller; existing OutState, Timeline, throttled PLI.

**Produces:** `OutTrack`da explicit bandwidth pause va keyframe kutish holati;
`allocate_layers` TWCC event, subscription/mute o‘zgarishi va housekeeping’dan
chaqiriladi. Negotiation status bandwidth pause’dan mustaqil qoladi.

- [ ] RED testlar: muted/o‘z/so‘ralmagan audio hisoblanmasligi; mikrofon
  limiti va screen_audio; speaker priority almashishi; subscription quality
  pasayishi pending upgrade’ni bekor qilishi.
- [ ] Forwarding gate testlari: paused video nol media write; resume’dan
  keyingi delta frame tashlanadi, tanlangan keyframe o‘tadi; single encoding
  va oldingi RID’ga qaytish uchun ham shunday; ovoz bloklanmaydi.
- [ ] `cargo test --lib room::` bilan RED’ni tasdiqlash.
- [ ] Room’da eligible audio va video context yig‘ish. Existing BWE handler’da
  yangi bahoni saqlab allocation’ni qayta hisoblash. Housekeeping fallback’ini
  saqlash; mute/unpublish yo‘llari context’ni yangilashini tekshirish.
- [ ] Selected target/paused holatini forwarding’ga qo‘llash. Downgrade’da
  sig‘maydigan eski qatlamni darhol to‘xtatish; upgrade’da eski qatlam faqat
  mos yangi keyframe kelguncha davom etishi. Resume uchun throttled PLI,
  Timeline continuity va pause paytida negotiation saqlanishini implement qilish.
- [ ] Qaror o‘zgargandagina debug log: estimate, budget, track, allocation,
  pause/resume/layer-change sababi. Har frame log yozilmasin.
- [ ] `cargo test --lib` PASS; existing Timeline/keyframe tests regressiyasiz.
- [ ] Commit: `feat: pause and resume SFU video safely under congestion`.

### Task 4: Real media va transport tekshiruvi

**Files:** `server/src/room/adaptation_tests.rs`, `server/src/bot.rs`,
`server/tests/media.rs`, `docs/ARCHITECTURE.md`, `docs/TESTING.md`, `README.md`.

**Consumes:** Real room/Bot ICE + DTLS-SRTP; Task 3 integrated allocation.

**Produces:** Production override’siz, cfg(test) ichida boshqariladigan room
harness va har bir tekshiruvning aniq chegarasi yozilgan natijalar.

- [ ] Bot synthetic video payload’da qatlam marker’ini ajratish va BotStats’da
  har publisher/qatlam bo‘yicha olingan frame hisobini qo‘shish; marker o‘qish
  xavfsizligini unit test bilan tekshirish. Existing testlar saqlansin.
- [ ] Room module ichidagi cfg(test) harness haqiqiy room/peer driving’dan
  foydalansin; test-only bahoni TWCC hodisasidan keyin allocation oldida
  qo‘llasin. Production command, HTTP, WS yoki config override qo‘shilmasin.
- [ ] RED media test: bitta simulcast publisher va uch subscriber uchun
  300/800/2500 kbit/s bahoda mos ravishda q/h/f kadrlar kelishi, begona
  qatlam kadrlariga ruxsat berilmasligi. Startup/upgrade/keyframe settle
  tugagandan keyin bounded kuzatuv oynasida tekshirish.
- [ ] RED test: faqat bir subscriber bahosini nolga tushirish uning videosini
  to‘xtatsin, audiosi va boshqa ikkisining videosi davom etsin; 2500 ga
  tiklashdan keyin 2 soniya + keyframe muddatida video qaytsin.
- [ ] `cargo test --lib room::adaptation_tests` va `cargo test --test media`
  bilan RED/GREEN natijalarni olish; yangi finite startup budget sabab eski
  media testlarining kutilmalari o‘zgarsa faqat zarur joylarni moslash.
- [ ] Shu str0m versiyasidagi BWE probing/padding API va holatini dependency
  manbasidan tekshirish. Desired bitrate pauzadagi nol media rate bilan
  nolga tushmasin; pause’da ham transport signalini saqlashni tasdiqlash.
- [ ] User-space UDP relay yoki mavjud test transportida bitta subscriber
  uchun real packet pacing/drop cheklovi bilan TWCC pasayishi va tiklanishini
  sinash; cheklangan va cheklanmagan yo‘llarni alohida o‘lchash. Faqat
  bahoni inject qilish bu qadamni bajarmaydi. Muhit imkon bermasa, aynan
  qaysi cheklov va qaysi sinov bajarilmagani TESTING.md’da qayd etilsin.
- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test` muvaffaqiyatli tugasin. Faqat SDK o‘zgarsa Flutter tekshiruvlari.
- [ ] Hujjatlarda yangi siyosat, nominal xarajat chegarasi, real TWCC va
  synthetic media sinovlari farqini yozish; production deploy bajarilmasin.
- [ ] `git diff --check` va to‘liq o‘zgarishlarni ko‘rib chiqish; spec
  mezonlari bo‘yicha bajarilgan/bajarilmagan dalillarni yakuniy javobda aytish.
- [ ] Commit: `test: verify adaptive SFU media and recovery`.

## Execution Review

Reja o‘zini tekshirish: spec’dagi barcha mezonlar Tasks 1–4’da qamrab olingan;
allocator/controller interfeyslari bir xil; vaqt testlari fake Instant bilan;
test-only baho production’da mavjud emas. Transport TWCC va real dekoder
tekshiruvi synthetic frame testlaridan alohida hisobot qilinadi.

Tavsiya etilgan bajarish usuli: **Native** — vazifalar bir xil allocator,
controller va OutTrack interfeyslariga ketma-ket bog‘langan. Rejani foydalanuvchi
ko‘rib ma’qullagandan keyin `superpowers:executing-plans` bilan bajariladi.
