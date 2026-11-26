import CallKit
import Flutter
import UIKit

/// Reports `{"locked": Bool, "call": Bool}` while somebody listens.
public class EcochainRtcPlugin: NSObject, FlutterPlugin, FlutterStreamHandler, CXCallObserverDelegate {
  private var sink: FlutterEventSink?
  private var observers: [NSObjectProtocol] = []
  private var calls: CXCallObserver?
  private var locked = false

  public static func register(with registrar: FlutterPluginRegistrar) {
    let channel = FlutterEventChannel(
      name: "ecochain_rtc/interruptions", binaryMessenger: registrar.messenger())
    channel.setStreamHandler(EcochainRtcPlugin())
    SystemCall.shared.attach(
      FlutterMethodChannel(name: "ecochain_rtc/call", binaryMessenger: registrar.messenger()))
  }

  public func onListen(withArguments arguments: Any?, eventSink: @escaping FlutterEventSink) -> FlutterError? {
    stop()
    sink = eventSink
    locked = false
    let calls = CXCallObserver()
    calls.setDelegate(self, queue: .main)
    self.calls = calls
    // iOS has no public "screen locked" event. Protected data goes away a few
    // seconds after locking a device with a passcode; a dark screen when the
    // app leaves the foreground usually tells sooner.
    on(UIApplication.protectedDataWillBecomeUnavailableNotification) { [weak self] in self?.setLocked(true) }
    on(UIApplication.didEnterBackgroundNotification) { [weak self] in
      if UIScreen.main.brightness == 0 { self?.setLocked(true) }
    }
    on(UIApplication.didBecomeActiveNotification) { [weak self] in self?.setLocked(false) }
    send()
    return nil
  }

  public func onCancel(withArguments arguments: Any?) -> FlutterError? {
    stop()
    return nil
  }

  public func callObserver(_ callObserver: CXCallObserver, callChanged call: CXCall) {
    send()
  }

  private func on(_ name: Notification.Name, _ handler: @escaping () -> Void) {
    observers.append(
      NotificationCenter.default.addObserver(forName: name, object: nil, queue: .main) { _ in handler() })
  }

  private func stop() {
    observers.forEach(NotificationCenter.default.removeObserver)
    observers = []
    calls?.setDelegate(nil, queue: nil)
    calls = nil
    sink = nil
  }

  private func setLocked(_ value: Bool) {
    locked = value
    send()
  }

  private func send() {
    // Ringing counts as a call too; our own meeting does not.
    let own = SystemCall.shared.uuid
    let call = calls?.calls.contains { !$0.hasEnded && $0.uuid != own } ?? false
    sink?(["locked": locked, "call": call])
  }
}
