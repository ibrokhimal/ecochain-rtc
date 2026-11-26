import AVFoundation
import CallKit
import Flutter
import UIKit
import WebRTC

/// The running meeting as a CallKit call. Main thread only.
final class SystemCall: NSObject, CXProviderDelegate {
  static let shared = SystemCall()

  /// The CallKit call of the meeting, if any.
  private(set) var uuid: UUID?

  private var channel: FlutterMethodChannel?
  private var connected = false
  private var connectedReported = false
  private var started = false
  private var muted = false
  private var held = false
  /// Actions requested by the app itself, not by the user on a system screen.
  private var ownActions = Set<UUID>()
  private let controller = CXCallController()

  private lazy var provider: CXProvider = {
    let configuration = CXProviderConfiguration()
    configuration.supportsVideo = true
    configuration.maximumCallGroups = 1
    configuration.maximumCallsPerCallGroup = 1
    configuration.supportedHandleTypes = [.generic]
    // A tap in Recents could not rejoin the room.
    configuration.includesCallsInRecents = false
    let provider = CXProvider(configuration: configuration)
    provider.setDelegate(self, queue: .main)
    return provider
  }()

  func attach(_ channel: FlutterMethodChannel) {
    self.channel = channel
    channel.setMethodCallHandler { [weak self] call, result in
      switch call.method {
      case "show":
        self?.show(call.arguments as? [String: Any] ?? [:])
        result(nil)
      case "end":
        self?.end()
        result(nil)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
    NotificationCenter.default.addObserver(
      self, selector: #selector(didBecomeActive), name: UIApplication.didBecomeActiveNotification, object: nil)
  }

  private func show(_ state: [String: Any]) {
    connected = state["connected"] as? Bool ?? false
    muted = state["muted"] as? Bool ?? false
    guard let uuid = uuid else {
      start(title: state["title"] as? String ?? "")
      return
    }
    guard started else { return }
    reportConnected()
    syncMuted(uuid)
  }

  private func start(title: String) {
    let id = UUID()
    uuid = id
    started = false
    connectedReported = false
    held = false
    _ = provider
    let action = CXStartCallAction(call: id, handle: CXHandle(type: .generic, value: title))
    action.isVideo = true
    request(action) { [weak self] error in
      guard let self = self, self.uuid == id else { return }
      if error != nil {
        // The meeting goes on without a system call.
        return
      }
      self.started = true
      let update = CXCallUpdate()
      update.localizedCallerName = title
      update.hasVideo = true
      update.supportsHolding = true
      update.supportsGrouping = false
      update.supportsUngrouping = false
      update.supportsDTMF = false
      self.provider.reportCall(with: id, updated: update)
      self.reportConnected()
      self.syncMuted(id)
    }
  }

  private func reportConnected() {
    guard let uuid = uuid, connected, !connectedReported else { return }
    connectedReported = true
    provider.reportOutgoingCall(with: uuid, connectedAt: nil)
  }

  private var systemMuted = false

  private func syncMuted(_ uuid: UUID) {
    guard muted != systemMuted else { return }
    systemMuted = muted
    request(CXSetMutedCallAction(call: uuid, muted: muted))
  }

  private func end() {
    guard let uuid = uuid else { return }
    let wasStarted = started
    reset()
    if wasStarted {
      request(CXEndCallAction(call: uuid)) { [weak self] error in
        if error != nil { self?.provider.reportCall(with: uuid, endedAt: nil, reason: .remoteEnded) }
      }
    } else {
      // The start transaction may still be on its way.
      provider.reportCall(with: uuid, endedAt: nil, reason: .failed)
    }
  }

  private func reset() {
    uuid = nil
    started = false
    held = false
    systemMuted = false
  }

  private func request(_ action: CXCallAction, _ done: ((Error?) -> Void)? = nil) {
    ownActions.insert(action.uuid)
    controller.request(CXTransaction(action: action)) { [weak self] error in
      DispatchQueue.main.async {
        if let error = error {
          NSLog("ecochain_rtc: CallKit %@: %@", String(describing: type(of: action)), error.localizedDescription)
          self?.ownActions.remove(action.uuid)
        }
        done?(error)
      }
    }
  }

  /// iOS does not always resume a held call when the other one ends.
  @objc private func didBecomeActive() {
    guard held, let uuid = uuid else { return }
    let others = controller.callObserver.calls.contains { !$0.hasEnded && $0.uuid != uuid }
    if !others {
      controller.request(CXTransaction(action: CXSetHeldCallAction(call: uuid, onHold: false))) { _ in }
    }
  }

  // MARK: CXProviderDelegate

  func providerDidReset(_ provider: CXProvider) {
    guard uuid != nil else { return }
    reset()
    ownActions.removeAll()
    channel?.invokeMethod("hangUp", arguments: nil)
  }

  func provider(_ provider: CXProvider, perform action: CXStartCallAction) {
    ownActions.remove(action.uuid)
    guard action.callUUID == uuid else {
      action.fail()
      return
    }
    action.fulfill()
    provider.reportOutgoingCall(with: action.callUUID, startedConnectingAt: nil)
  }

  func provider(_ provider: CXProvider, perform action: CXEndCallAction) {
    let own = ownActions.remove(action.uuid) != nil
    if !own, action.callUUID == uuid {
      reset()
      channel?.invokeMethod("hangUp", arguments: nil)
    }
    action.fulfill()
  }

  func provider(_ provider: CXProvider, perform action: CXSetMutedCallAction) {
    let own = ownActions.remove(action.uuid) != nil
    if !own, action.callUUID == uuid {
      systemMuted = action.isMuted
      channel?.invokeMethod("mute", arguments: action.isMuted)
    }
    action.fulfill()
  }

  func provider(_ provider: CXProvider, perform action: CXSetHeldCallAction) {
    if action.callUUID == uuid {
      held = action.isOnHold
      channel?.invokeMethod("hold", arguments: action.isOnHold)
    }
    action.fulfill()
  }

  // CallKit owns the audio session during a call; WebRTC has to know.
  func provider(_ provider: CXProvider, didActivate audioSession: AVAudioSession) {
    RTCAudioSession.sharedInstance().audioSessionDidActivate(audioSession)
  }

  func provider(_ provider: CXProvider, didDeactivate audioSession: AVAudioSession) {
    RTCAudioSession.sharedInstance().audioSessionDidDeactivate(audioSession)
  }
}
