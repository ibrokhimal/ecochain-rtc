# ecochain_rtc

Ecochain RTC media serveriga ulanadigan Flutter SDK. Ekranlarsiz: UI uchun `ecochain_meeting_ui` paketiga qarang.

Qo‘llanadigan platformalar: Android, iOS, macOS, Windows (`flutter_webrtc` 1.6.3 orqali). Sinov holati: [docs/TESTING.md](../../docs/TESTING.md#platformalar).

## O‘rnatish

```yaml
dependencies:
  ecochain_rtc:
    path: ../packages/ecochain_rtc
```

### Platforma sozlamalari

* **iOS** `Info.plist`: `NSCameraUsageDescription`, `NSMicrophoneUsageDescription`; fonda audio uchun `UIBackgroundModes: [audio]`.
* **macOS** `Info.plist` — yuqoridagi ikki kalit; `*.entitlements`: `com.apple.security.device.camera`, `com.apple.security.device.audio-input`, `com.apple.security.network.client`.
* **Android** `AndroidManifest.xml`: `INTERNET`, `CAMERA`, `RECORD_AUDIO`, `MODIFY_AUDIO_SETTINGS`, `ACCESS_NETWORK_STATE`, `CHANGE_NETWORK_STATE`, `BLUETOOTH_CONNECT`. Development’da `ws://` uchun faqat `src/debug` manifestida `usesCleartextTraffic="true"`.
* **Windows** — qo‘shimcha sozlama talab qilinmaydi.

`apps/meet` va `apps/messenger_demo` tayyor namunalar.

## Asosiy foydalanish

```dart
import 'package:ecochain_rtc/ecochain_rtc.dart';

// Sessiyani ekranda emas, ilova darajasida saqlang.
final session = RtcSession();

Future<void> join(String token) async {
  await RtcSession.requestPermissions(microphone: true, camera: true);
  try {
    await session.connect(RtcConnectOptions(
      url: Uri.parse('wss://meet.ecochain.uz/v1/rtc'),
      token: token,
      tokenProvider: () => backend.fetchMeetingToken('team-42'), // reconnect’da token eskirsa
      microphone: true,
      camera: true,
    ));
  } on RtcException catch (e) {
    switch (e.code) {
      case RtcErrorCode.roomFull: showMessage('Xona to‘lgan');
      case RtcErrorCode.roomEnded: showMessage('Uchrashuv tugagan');
      default: showMessage('Ulanib bo‘lmadi');
    }
  }
}
```

### Holat va ishtirokchilar

```dart
ListenableBuilder(
  listenable: session,
  builder: (context, _) {
    if (session.connectionState == RtcConnectionState.reconnecting) {
      return const Text('Aloqa tiklanmoqda…');
    }
    return GridView.count(
      crossAxisCount: 2,
      children: [
        for (final p in session.participants)
          RtcVideoView(
            key: ValueKey(p.id),          // id reconnect’da o‘zgarmaydi
            track: p.camera,
            mirror: p.isLocal,
            placeholder: Center(child: Text(p.name)),
          ),
      ],
    );
  },
);
```

### Video codec va simulcast

SDK kamera va ekran transceiver'larida H.264 `packetization-mode=1` ni
birinchi tanlov qiladi: constrained baseline, keyin boshqa H.264 profillari,
keyin VP8. Qurilma bergan capability va fmtp qiymatlari o‘zgartirilmaydi;
RTX/RED/FEC hamda qolgan codeclar muzokarada saqlanadi. Codec tanlash API’si
ishlamasa, WebRTC platformasining odatiy tanlovi ishlaydi. Bu preference;
haqiqiy tanlangan codec’ni debug statistikasi ko‘rsatadi.

Server ham publisher SDP javobida shu tartibni qo‘llaydi: str0m o‘zining
codec tartibidan javob yaratadi, shuning uchun faqat app preference’i yetmaydi.
Bu o‘zgarish uchun serverni ham yangilash kerak. VP8 zaxirasi publisher
muzokarasi uchun; H.264 yuborilayotgan oqimni server VP8-only receiver uchun
VP8 ga aylantirmaydi.

Kamera odatdagidek qurilmaning o‘zida q/h/f qatlamlarini yaratadi
(1/4, 1/2, to‘liq o‘lcham; 150/500/1500 kbit/s maksimum).
Server videoni decode/encode qilmaydi: qabul qiluvchining tezligiga mos
tayyor qatlamni uzatadi. Bu H.264 simulcast, spatial SVC emas.
FFmpeg kerak emas. Native qurilmaning uchta H.264 encoder oqimini real
vaqtda bera olishi qurilma testi bilan tekshiriladi; codec mavjudligi
hardware tezlatish yoki uchta qatlam ishlayotganining isboti emas.

### Video debug statistikasi

`flutter run --debug` bilan `RtcVideoView` har remote video ustida codec,
haqiqiy frame o‘lchami, FPS va qabul qilinayotgan video bitrate’ini chiqaradi:

```text
RX • H264 • 640×360
25 FPS • 480 kbit/s
```

Ko‘rsatkichlar bir soniyada yangilanadi; FPS va bitrate counter farqidan
hisoblanadi. Birinchi o‘lchov yoki platforma bermagan qiymat `—` bo‘ladi.
Lokal preview `LOCAL` deb belgilanadi. Polling faqat debug build’da, tegishli
tile ochiq va muted bo‘lmaganda ishlaydi; tile yopilganda to‘xtaydi.

### Qurilmalar

```dart
await session.setMicrophoneEnabled(false);   // capture to‘xtaydi, tanlov reconnect’da saqlanadi
await session.setCameraEnabled(true);
await session.switchCamera();                 // mobil
await session.startScreenShare();             // macOS/Windows: birinchi ekran
await session.stopScreenShare();
```

Chaqiruvlar navbatga qo‘yiladi: tez-tez bosish ikkinchi kamerani ochmaydi, oxirgi so‘ralgan holat qoladi.

### Qulf va telefon qo‘ng‘irog‘i

Android va iOS’da ekran qulflansa yoki telefonga qo‘ng‘iroq kelsa, SDK mikrofon, kamera va ekranni o‘zi bo‘shatadi (boshqalarga “muted” ko‘rinadi). Ilova yana oldinga chiqqanda foydalanuvchi tanlovi tiklanadi; ekran ulashish tiklanmaydi. Qabul qilish davom etadi. Holat: `session.sendingPaused`, `session.sendPauseReasons`.

* iOS’da qulf uchun rasmiy hodisa yo‘q: ba’zan qulflangandan bir necha soniya keyin, parolsiz qurilmada esa umuman aniqlanmasligi mumkin. Qo‘ng‘iroqlar `CXCallObserver` orqali.
* Android’da qo‘ng‘iroq audio rejimidan aniqlanadi (ruxsat kerak emas). Ovozsiz rejimda u faqat javob berilganda seziladi; boshqa VoIP ilovalar (Telegram, WhatsApp) qo‘ng‘irog‘i sezilmaydi.

### Tizim qo‘ng‘irog‘i (qulf ekrani)

`RtcConnectOptions(systemCall: RtcSystemCall(title: 'Jamoa uchrashuvi'))` berilsa, uchrashuv telefonning o‘z qo‘ng‘irog‘idek ko‘rinadi: iOS’da CallKit, Android’da foreground service va `CallStyle` bildirishnomasi (Android 12+; eskisida oddiy bildirishnoma). `title` qulf ekranida hammaga ko‘rinadi. Qulfda mikrofon va kamera baribir yopiq turadi va u yerdan yoqib bo‘lmaydi.

* **iOS**: `UIBackgroundModes` ga `audio` bilan birga `voip` kerak. Boshqa qo‘ng‘iroq “hold” qilsa, yuborish to‘xtaydi.
* **Android**: ruxsatlar va service plugin manifestidan keladi; Android 13+ da bildirishnoma ruxsati ulangandan keyin so‘raladi. Google Play’da foreground service turlari (`microphone`, `mediaPlayback`) deklaratsiya qilinishi kerak.

Ilova yopiq paytda jiringlatish (PushKit/FCM) kirmaydi.

### Hodisalar

```dart
session.events.listen((event) {
  switch (event) {
    case RtcParticipantJoined(:final participant): toast('${participant.name} qo‘shildi');
    case RtcSessionEnded(reason: RtcDisconnectReason.roomEnded): showEnded();
    case RtcSessionEnded(reason: RtcDisconnectReason.removed): showRemoved();
    default:
  }
});
```

### Qaysi videolarni qabul qilish

Xonaga 25 kishi kirishi mumkin, lekin telefon 4 ta videoni dekodlaydi (desktop 9). Ekranda ko‘rinayotgan tile’larni bildiring:

```dart
session.setVideoLayout(
  visibleParticipantIds: visibleTiles.map((p) => p.id).toList(),
  pinnedParticipantId: pinned?.id,
);
```

Ustuvorlik: ekran ulashish → mahkamlangan → faol gapiruvchilar → ko‘rinib turganlar (`RtcVideoSubscriptionPolicy`).

### Mezbon

```dart
if (session.isHost) {
  await session.removeParticipant(participantId);
  await session.endMeeting();
}
```

### Chiqish

```dart
await session.leave();   // kamera, mikrofon, PeerConnection’lar, socket bo‘shatiladi
session.dispose();       // obyekt endi kerak bo‘lmaganda
```

## Hayot sikli va kafolatlar

* `connect` jarayonda yoki ulangan bo‘lsa, ikkinchi chaqiruv o‘sha future’ni qaytaradi — ikkinchi qo‘ng‘iroq yaratilmaydi.
* `leave`dan keyin kelgan eski asinxron natijalar (join javobi, kamera ochilishi, PeerConnection) sessiyani qayta faollashtirmaydi; yaratilgan resurslar darhol yopiladi.
* Tarmoq uzilsa: `reconnecting`, eksponensial backoff + jitter (0,5 → 8 s), `giveUpAfter` (60 s) dan keyin `networkFailed`. Fatal holatlar (`roomEnded`, `removed`, `replacedByOtherSession`, `unauthorized`, `roomFull`) retry qilinmaydi.
* ICE `disconnected/failed` 2 s davom etsa ICE restart; muvaffaqiyatsiz bo‘lsa ikkala peer qayta quriladi.
* Thread-safety: barcha metodlar Flutter UI isolate’idan chaqirilishi kerak.

## Xavfsizlik

* Faqat `wss://`. `ws://` faqat `allowInsecureLocalhost: true` va loopback/xususiy LAN manzil uchun (development). Sertifikat tekshiruvini o‘chirish imkoniyati yo‘q.
* Media DTLS-SRTP bilan shifrlanadi; server SFU sifatida media’ni ochiq ko‘radi (end-to-end emas).

## Yozib olish va fayllar (keyingi bosqich)

Hozirgi SDK faqat jonli audio-video beradi. Yozib olish serverda (SFU allaqachon media’ni oladi) alohida bosqichda qo‘shiladi: har fayl alohida kalit bilan shifrlanadi, kalitlar va har `fileId`ga ruxsat server tomonda tekshiriladi. Mobil ilovaga ta’siri: mavjud API o‘zgarmaydi; yozib olish holati hodisasi va fayl ro‘yxati/yuklab olish API’si qo‘shiladi. Ilovalar fayl kalitlarini hech qachon ko‘rmaydi.
