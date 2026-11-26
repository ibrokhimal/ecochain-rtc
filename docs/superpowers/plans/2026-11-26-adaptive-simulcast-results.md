# Adaptiv simulcast — bajarish natijalari

Sana: 2026-11-26. Usul: Native, bitta yakuniy mustaqil code review bilan.
Baseline: `98c0e9a`. Branch: `feat/adaptive-simulcast`.

## Mahsulot xatti-harakati

Flutter’ning mavjud q/h/f kamera encoding’lari saqlandi. Rust SFU har bir
qabul qiluvchiga alohida TWCC budjeti, audio zaxirasi va requested quality
asosida qatlam tanlaydi. Sig‘magan video SDP subscription’ni yopmasdan
pauza qilinadi; tiklanish keyframe bilan ochiladi. Pasayish darhol,
yaxshilanish 2 s barqaror 20% zaxira bilan amalga oshadi.

Barcha requested qatlamlar uzatilgach, recovery minimum padding o‘chadi;
desired probe maqsadi qoladi. Yangi signaling versiyasi va yangi SDK
majburiy maydoni kerak emas. Bu uch encoding’li simulcast, spatial SVC emas.

## Dalillar

- `cargo fmt --check`: PASS.
- `cargo clippy --all-targets -- -D warnings`: PASS.
- `cargo test -- --test-threads=1`: **119 PASS, 0 FAIL**.
- Suite tarkibi: 86 unit, 14 media, 6 resilience, 13 signaling.
- Baseline’da 59 unit test o‘tgan; yangi xatti-harakatlar RED → GREEN bilan
  tekshirildi, shu jumladan real media/transport regressiyalari.
- Uch receiver: cfg(test) baholari 300/800/2500 kbit/s; real ICE/DTLS-SRTP
  orqali mos q/h/f kadrlar. Bitta receiver pauzasi boshqalariga ta’sir qilmaydi.
- Baho inject qilinmagan UDP relay: 4 Mbit/s → 120 kbit/s → 4 Mbit/s;
  real TWCC pasayishi, to‘liq video pauza, audio davom etishi va yuqori sifat
  tiklanishi tekshirildi. Single encoding resume ham tekshirildi.
- Test relay faqat downlink SRTP/padding’ni cheklaydi; boshqa trafik erkin.
  Synthetic kadrlar real dekoder yoki mobil kamera tasvirining isboti emas.
- O‘zboshimchalik bilan bandwidth override qilish uchun production endpoint
  qo‘shilmadi: test command, relay va write hisoblagichi cfg(test) ostida.

## Mustaqil ko‘rik va tuzatishlar

Yakuniy reviewer beshta Important holat topdi; Critical topilmadi.
Har bir holat RED regressiya bilan tasdiqlanib bitta fix pass’da tuzatildi:

1. Downgrade/resume tugamasdan upgrade boshlansa, eski taqiqlangan RID qayta
   ochilardi. Endi faqat uzluksiz ruxsat etilgan eski qatlam davom etadi.
2. Boshqa offer pending bo‘lsa unsubscribe qilingan video ham uzatilardi.
   Endi amaldagi subscription membership darhol tekshiriladi.
3. Bitta nomli RID 150 kbit/s deb baholanardi. Endi single kamera 800,
   single ekran 1500 kbit/s deb olinadi.
4. Fully active video ham doimiy recovery padding bilan link’ni to‘ldirardi.
   Real wire test tuzatishdan oldin 300 kbit/s kamera uchun 1,206,415 bit/s
   o‘lchadi; tuzatishdan keyin 900 kbit/s chegaradan past bo‘lib o‘tdi.
5. Writer keyframe’ni enqueue qila olmasa ham gate ochilardi. Endi gate va
   timeline holati faqat muvaffaqiyatli write’dan keyin commit qilinadi.

Ko‘rikdan keyin ikkinchi reviewer chaqirilmadi: tuzatishlarni regressiyalar
va yuqoridagi to‘liq green suite tasdiqladi.

## Qabul qilingan qarorlar va chegaralari

| Qaror | Sabab | Chegara yoki xavf |
| --- | --- | --- |
| Mavjud checkout, yangi feature branch | Desktop ko‘rsatmasi va baseline commit talabi | Parallel tashqi fayl tahrirlaridan filesystem izolyatsiyasi yo‘q |
| Yetarli zaxira saqlangan mayda TWCC pasayishi timer’ni reset qilmaydi | Aks holda yetarli tezlikda ham recovery cheksiz kechikardi | Mayda pasayish davom etayotganda ham 20% zaxira ichida upgrade mumkin |
| 20% recovery zaxirasi taqsimotdan oldin ajratiladi | Sig‘maydigan high upgrade boshqa kameraning bazaviy sifatini bloklamasin | Yangi kamera uchun oldingi high sifat vaqtincha pasayishi mumkin |
| Fizik qurilma dekoder sinovi alohida | Tasdiqlangan scope — SFU va real transport ustida synthetic RTP | Hardware codec/rendering xatti-harakati bu ishda tekshirilmagan |
| Production deploy alohida | Ushbu topshiriqda lokal repository o‘zgartirildi | Jonli xizmat deploy qilinmaguncha eski kodda ishlaydi |

## Keyinga qoldirilgan kichik optimizatsiya

`audio_demand_counts` har TWCC update’da outbound treklarni qayta qidiradi.
Audio-only filter yoki wanted audio ID indeksi hisoblashni kamaytirishi
mumkin. Review buni Minor deb baholadi; performance regressiyasi o‘lchanmadi.
Katta xonalar uchun yangi load benchmark bu bosqichda bajarilmadi.

## Integratsiya holati

O‘zgarishlar lokal feature branch’da saqlandi. Merge, remote push va
production rollout bajarilmadi. SDK/ilovalarning baseline’dagi oldingi
mobil UI/CallKit o‘zgarishlari saqlandi va qayta yozilmadi.
