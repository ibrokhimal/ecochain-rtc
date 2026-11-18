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

### Qurilmalar

```dart
await session.setMicrophoneEnabled(false);   // capture to‘xtaydi, tanlov reconnect’da saqlanadi
await session.setCameraEnabled(true);
await session.switchCamera();                 // mobil
await session.startScreenShare();             // macOS/Windows: birinchi ekran
await session.stopScreenShare();
```

Chaqiruvlar navbatga qo‘yiladi: tez-tez bosish ikkinchi kamerani ochmaydi, oxirgi so‘ralgan holat qoladi.

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
