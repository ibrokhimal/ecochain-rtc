# Internet tezligiga moslashuvchi meeting videosi

Sana: 2026-11-26
Holat: foydalanuvchi arxitektura yo‘nalishini ma’qullagan; yozma dizayn ko‘rib chiqish uchun tayyor.

## Maqsad va kelishilgan yo‘l

Har bir qatnashuvchi uchun alohida downlink budjeti asosida kamera sifatini
pasaytirish va qayta oshirish. Zaif tarmoqda ovozni va muhim videoni ustuvor
saqlash. Foydalanuvchi mavjud uch qatlamli simulcast’ni yaxshilash tavsiyasiga
rozi bo‘ldi. Ushbu bosqich haqiqiy spatial SVC codec integratsiyasini o‘z ichiga
olmaydi va Zoom bilan barcha imkoniyatlarda tenglikni da’vo qilmaydi.

## Hozirgi tizim va muammo

- `packages/ecochain_rtc/lib/src/session.dart` kamera uchun q/h/f RID’larini
  4x/2x/1x kichraytirish va 150/500/1500 kbit/s maksimal bitrate bilan yuboradi.
- Rust server `str0m` 0.25.0 orqali har subscriber uchun TWCC bahosini oladi.
- `server/src/sfu/layers.rs::allocate` ekran va yuqori sifat talablarini
  ustuvor qiladi, ammo barcha treklarning eng past qatlamini budjetdan qat’i
  nazar yuboradi. Audio va transport xarajatlari budjetdan ayrilmaydi.
- Boshlang‘ich TWCC bahosi yo‘qligida allocator so‘ralgan barcha sifatlarni
  beradi, garchi subscriber BWE 800 kbit/s bilan boshlansa ham.
- Qatlam almashish keyframe, mavjud PLI throttle va RTP timeline orqali
  boshqariladi. Vaqtincha pauzadan keyin esa alohida keyframe gate kerak.

## Variantlar va tanlov

1. **Tanlangan: mavjud simulcast va adaptiv SFU.** SDK’dagi uch encoding va
   signaling saqlanadi; asosiy o‘zgarish allocator, forwarding va tekshiruvlarda.
2. **Haqiqiy VP9/AV1 SVC.** Codec/platform capability aniqlash, qatlamlar
   bog‘liqligini hisobga oladigan forwarding va simulcast fallback talab qiladi.
   Alohida keyingi loyiha; ushbu ishni bajarish sharti emas.
3. **Serverda transcoding.** Har videoni serverda qayta encode qilish CPU
   xarajati va kechikishni oshiradi; mavjud uch qatlam uchun kerak emas.

## Budjet va sifat siyosati

Hisoblash har bir subscriber uchun mustaqil bajariladi. TWCC bo‘lmaganda
800 kbit/s boshlang‘ich limit qo‘llanadi. Nol baho cheklanmagan tezlik emas.

Video budjeti: `max(0, floor(estimate_kbps * 0.85) - audio_reserve_kbps)`.
15 foiz transport/RTX va baho o‘zgarishlari uchun zaxira hisoblanadi; bu
haqiqiy wire bitrate’ning qat’iy kafolati emas.

Audio zaxirasi har faol yetkazilishi mumkin bo‘lgan audio trek uchun
64 kbit/s. Mikrofonlar soni mavjud `max_forwarded_audio` limiti bilan
cheklanadi; ekran audiosi alohida hisoblanadi. O‘z audiosi, muted treklar
va yetkazilishi so‘ralmagan treklar hisobga olinmaydi. Ovozning o‘zi ham
sig‘maydigan tarmoqda video pauza bo‘ladi; ovoz sifati kafolatlanmaydi.

Simulcast qatlamlari uchun boshlang‘ich xarajat modeli 150/500/1500 kbit/s.
Bitta encoding kamera 800, ekran videosi 1500 kbit/s deb hisoblanadi,
SDK’dagi amaldagi limitlarga mos. Keyingi o‘lchovlar bu konservativ modelni
o‘zgartirishga asos bo‘lishi mumkin.

Taqsimot ikki bosqichli:

1. Ustuvorlik bo‘yicha eng past sifatga joy ajratish; sig‘magan video pauza.
2. Qolgan budjet bilan shu tartibda so‘ralgan sifatgacha yaxshilash.

Ustuvorlik: ekran ulashish, high sifat so‘ralgan kamera, faol gapiruvchi,
qolgan kameralar. Har trek uchun klient so‘ragan sifat yuqori chegara.
Teng ustuvorlikda barqaror tartib saqlanadi, shunda qayta hisoblash
videolarni tasodifiy almashtirmaydi. Ekranning bitta encoding’i sig‘masa,
u pauza bo‘ladi; sig‘adigan past sifatli kameralar ishlashi mumkin.

Allocator natijasi aniq `Paused | Layer(index)` bo‘ladi. `None` RID
single-encoding trekni bildirishi mumkin, shu sabab pauza RID bilan
ifodalanmaydi. Pauza SDP subscription’ni yopmaydi.

## Vaqt bo‘yicha moslashish

- Yangi past TWCC bahosida budjet kamayishi keyingi allocation’da qo‘llanadi;
  BWE hodisasi allocation’ni ishga tushiradi, housekeeping zaxira yo‘l bo‘ladi.
- O‘sish yoki pauzadan qaytish uchun yangi taqsimot 2 soniya uzluksiz mos
  kelishi va uning narxi uchun kamida 20 foiz qo‘shimcha video budjeti bo‘lishi
  kerak. Past baho kelishi kutish vaqtini qayta boshlaydi.
- So‘ralgan sifat kamayishi, unsubscription yoki mute darhol bajariladi.
- Allocation kutayotgan yaxshilanishni hamda amaldagi forwarding holatini
  alohida saqlaydi. Limit kamayganda eski pending yaxshilanish bekor qilinadi.
- Subscriber peer qayta yaratilganda uning bahosi va kutish holati tozalanadi.

## Forwarding va qayta tiklanish

`OutTrack`da bandwidth pauzasi va keyframe kutish holati negotiation
holatidan alohida saqlanadi. Pauzadagi videoga media yozilmaydi, audioning
mavjud forwarding siyosati ishlashda davom etadi.

Pauzadan qaytishda yoki qatlam o‘zgarishida mavjud throttle orqali PLI
so‘raladi. Tanlangan encoding’ning keyframe’i kelmaguncha uning delta
kadrlariga yo‘l berilmaydi; bu single-encoding ekran uchun ham amal qiladi.
Downgrade vaqtida budjetga sig‘maydigan eski yuqori qatlam uzatilmaydi;
keyframe kelguncha qisqa vaqt tasvir to‘xtashi mumkin. Upgrade vaqtida
eski, budjetga sig‘adigan qatlam keyframe kelguncha davom etishi mumkin.
RTP vaqt uzluksizligi mavjud `Timeline` yordamida saqlanadi.

To‘liq pauzada ham BWE tiklanishni aniqlashi uchun `str0m` probing/bitrate
talabi faol saqlanadi. Joriy past media tezligi probing maqsadini nolga
tushirmaydi. Buni faqat allocator testi bilan isbotlash yetarli emas:
transport integratsion sinovi talab qilinadi.

## Klient va moslik

Flutter SDK’dagi mavjud uch kamera encoding’i saqlanadi. Yangi majburiy
signaling maydoni yoki qo‘lda sifat tanlash tugmasi talab qilinmaydi.
Server har subscriber uchun moslashadi; eski klient ham foyda oladi.
Pauzada klient oxirgi kadrni ko‘rsatishi mumkin. Maxsus “internet sust”
UI holati ushbu bosqich doirasiga kirmaydi.

Mavjud lokal, hali commit qilinmagan ilova/SDK o‘zgarishlari saqlanadi;
ushbu ish bilan bog‘liq bo‘lmagan fayllar o‘zgartirilmaydi.

## Kuzatuv va tekshirish

Allocation qarorlari debug log’da sabab, baho, video budjeti va tanlangan
qatlam/pauza bilan kuzatiladi. Har media kadriga log yozilmaydi.

Qabul qilish mezonlari:

1. Deterministik unit sinovlar: audio/overhead zaxirasi, nol/unknown budjet,
   barcha uch sifat, qisman yetadigan bazaviy budjet, priority, single encoding,
   sifat yuqori chegarasi, 2 soniyali upgrade gate va o‘zgaruvchan baholar.
2. Real ICE/DTLS-SRTP SFU testlari: bitta uch qatlamli publisher’dan uchta
   subscriber turli server-side baholar bilan turli qatlamlarni olishi;
   cheklangan bahoni kiritish faqat test harness’da mavjud bo‘ladi.
3. Bandwidth pasayganda past ustuvor video to‘xtashi, ovoz paketlari davom
   etishi; yaxshilanganda keyframe bilan qaytishi, qaytishdan oldin delta
   kadrlar uzatilmasligi. Single encoding qaytishi ham tekshiriladi.
4. Transport darajasida bandwidth cheklash bilan pasayish va tiklanishni
   sinash. Bahoni qo‘lda kiritish haqiqiy TWCC sinovi sifatida ko‘rsatilmaydi.
   To‘liq pauzadan tiklanish va ikki boshqa subscriber’ga ta’sir qilmaslik
   tekshiriladi. Muhit bu sinovga yo‘l bermasa, cheklov ochiq qayd qilinadi.
5. `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
   `cargo test`; SDK o‘zgarsa tegishli Flutter analyze/test ham bajariladi.

Nominal bitrate sinovlari haqiqiy dekoder tasvirini tasdiqlamaydi.
Qurilmada kamera bilan alohida tarmoq sinovi natijalari o‘lchangandan
keyingina hujjatga kiritiladi. Jonli production serverga deploy bu
dizaynni amalga oshirishdan alohida operatsiya hisoblanadi.
