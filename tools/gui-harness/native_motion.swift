// Native-window evidence for the production GUI. Compile with:
// swiftc -parse-as-library -O tools/gui-harness/native_motion.swift -o native-motion-recorder
// The Python driver validates the plan and provenance; this process only
// captures a real ScreenCaptureKit window and dispatches native input.
import AppKit
import ApplicationServices
import CoreImage
import CoreMedia
import Foundation
import ScreenCaptureKit

struct Action: Decodable {
    let at_ms: Int
    let kind: String
    let label: String
    let keycode: Int?
    let modifiers: [String]?
    let x: Double?
    let y: Double?
    let width: Double?
    let height: Double?
    let title: String?
    let role: String?
}
struct Plan: Decodable {
    let schema: Int
    let name: String
    let duration_ms: Int
    let max_frames: Int
    let window_id: UInt32?
    let capture_fps: Int?
    let capture_scope: String?
    let actions: [Action]
}

func jsonLine(_ value: [String: Any]) -> Data {
    let data = (try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])) ?? Data("{}".utf8)
    return data + Data([10])
}
func axValue(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success else { return nil }
    return value
}
func axElement(_ element: AXUIElement, _ name: String) -> AXUIElement? {
    guard let raw = axValue(element, name), CFGetTypeID(raw) == AXUIElementGetTypeID() else { return nil }
    return (raw as! AXUIElement)
}
func axString(_ element: AXUIElement, _ name: String) -> String? {
    axValue(element, name) as? String
}
func axRect(_ element: AXUIElement) -> [String: Double]? {
    guard let rawP = axValue(element, kAXPositionAttribute),
          let rawS = axValue(element, kAXSizeAttribute),
          CFGetTypeID(rawP) == AXValueGetTypeID(), CFGetTypeID(rawS) == AXValueGetTypeID() else { return nil }
    let p = rawP as! AXValue
    let s = rawS as! AXValue
    var point = CGPoint.zero
    var size = CGSize.zero
    guard AXValueGetValue(p, .cgPoint, &point), AXValueGetValue(s, .cgSize, &size) else { return nil }
    return ["x": point.x, "y": point.y, "width": size.width, "height": size.height]
}
func axNode(_ element: AXUIElement) -> [String: Any] {
    var node: [String: Any] = [:]
    if let role = axString(element, kAXRoleAttribute) { node["role"] = role }
    if let subrole = axString(element, kAXSubroleAttribute) { node["subrole"] = subrole }
    if let title = axString(element, kAXTitleAttribute) { node["title"] = title }
    if let description = axString(element, kAXDescriptionAttribute) { node["description"] = description }
    if let value = axValue(element, kAXValueAttribute) as? String { node["value"] = String(value.prefix(256)) }
    if let selected = axValue(element, kAXSelectedAttribute) as? NSNumber { node["selected"] = selected.boolValue }
    if let enabled = axValue(element, kAXEnabledAttribute) as? NSNumber { node["enabled"] = enabled.boolValue }
    if let rect = axRect(element) { node["bounds_pt"] = rect }
    return node
}
func axSnapshot(pid: pid_t, full: Bool) -> [String: Any] {
    let app = AXUIElementCreateApplication(pid)
    var result: [String: Any] = ["trusted": AXIsProcessTrusted()]
    if let focused = axElement(app, kAXFocusedUIElementAttribute) {
        result["focused"] = axNode(focused)
    }
    if let window = axElement(app, kAXFocusedWindowAttribute) {
        result["window"] = axNode(window)
    }
    if full {
        var nodes: [[String: Any]] = []
        var queue: [(AXUIElement, Int)] = [(app, 0)]
        var index = 0
        while index < queue.count && nodes.count < 256 {
            let (element, depth) = queue[index]
            index += 1
            var node = axNode(element)
            node["depth"] = depth
            nodes.append(node)
            if depth >= 8 { continue }
            if let children = axValue(element, kAXChildrenAttribute) as? [AXUIElement] {
                for child in children.prefix(64) where queue.count < 320 { queue.append((child, depth + 1)) }
            }
        }
        result["tree"] = nodes
        result["tree_truncated"] = index < queue.count
    }
    return result
}

final class Recorder: NSObject, SCStreamOutput, SCStreamDelegate {
    private let lock = NSLock()
    private let context = CIContext(options: [.useSoftwareRenderer: false])
    private let frames: FileHandle
    private let frameDir: URL
    private let pid: pid_t
    private let maxFrames: Int
    private var firstPTS: CMTime?
    private var count = 0
    private var dropped = 0
    private var lastFocus: [String: Any] = [:]
    private var failure: String?
    private var firstFrameTime: UInt64?
    init(frameDir: URL, frames: FileHandle, pid: pid_t, maxFrames: Int) {
        self.frameDir = frameDir
        self.frames = frames
        self.pid = pid
        self.maxFrames = maxFrames
    }
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        lock.lock(); failure = error.localizedDescription; lock.unlock()
    }
    func stream(_ stream: SCStream, sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, CMSampleBufferIsValid(sampleBuffer),
              let buffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        // Serialize the encode/write path. ScreenCaptureKit may call us on
        // different queues; no frame image is retained after this callback.
        lock.lock()
        defer { lock.unlock() }
        guard count < maxFrames else { dropped += 1; return }
        let pts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        guard pts.isValid else { dropped += 1; return }
        if firstPTS == nil { firstPTS = pts; firstFrameTime = DispatchTime.now().uptimeNanoseconds }
        let relativeMs = max(0, CMTimeGetSeconds(CMTimeSubtract(pts, firstPTS!)) * 1000)
        let width = CVPixelBufferGetWidth(buffer)
        let height = CVPixelBufferGetHeight(buffer)
        guard width > 0, height > 0 else { dropped += 1; return }
        let image = CIImage(cvPixelBuffer: buffer)
        guard let cg = context.createCGImage(image, from: image.extent),
              let png = NSBitmapImageRep(cgImage: cg).representation(using: .png, properties: [:]) else {
            dropped += 1; return
        }
        let file = String(format: "frame-%06d.png", count)
        do {
            try png.write(to: frameDir.appendingPathComponent(file), options: .atomic)
            let row: [String: Any] = ["index": count, "time_ms": relativeMs,
                "pts_seconds": CMTimeGetSeconds(pts), "width_px": width,
                "height_px": height, "file": "frames/" + file, "ax": lastFocus]
            frames.write(jsonLine(row))
            count += 1
        } catch { failure = "write frame: \(error)" }
    }
    // AX calls can synchronously message the app. Run them on an independent
    // sampler queue so a slow accessibility tree never holds the SCStream
    // callback or changes the recorded presentation cadence.
    func sampleAX() {
        let sampled = axSnapshot(pid: pid, full: false)
        lock.lock(); lastFocus = sampled; lock.unlock()
    }
    func status() -> (Int, Int, String?, UInt64?) {
        lock.lock(); defer { lock.unlock() }
        return (count, dropped, failure, firstFrameTime)
    }
}

func findAX(pid: pid_t, title: String, role: String?) throws -> AXUIElement {
    let app = AXUIElementCreateApplication(pid)
    var queue: [(AXUIElement, Int)] = [(app, 0)]
    var index = 0
    var found: [AXUIElement] = []
    while index < queue.count && index < 320 {
        let (element, depth) = queue[index]
        index += 1
        if (axString(element, kAXTitleAttribute) == title || axString(element, kAXDescriptionAttribute) == title),
           (role == nil || axString(element, kAXRoleAttribute) == role) {
            found.append(element)
        }
        if depth < 8, let children = axValue(element, kAXChildrenAttribute) as? [AXUIElement] {
            for child in children.prefix(64) where queue.count < 320 { queue.append((child, depth + 1)) }
        }
    }
    guard found.count == 1 else {
        throw NSError(domain: "native-motion", code: 8,
            userInfo: [NSLocalizedDescriptionKey: "AX target \(title) matched \(found.count) controls"])
    }
    return found[0]
}
func flags(_ modifiers: [String]?) throws -> CGEventFlags {
    var value: CGEventFlags = []
    for modifier in modifiers ?? [] {
        switch modifier {
        case "command": value.insert(.maskCommand)
        case "shift": value.insert(.maskShift)
        case "option": value.insert(.maskAlternate)
        case "control": value.insert(.maskControl)
        default: throw NSError(domain: "native-motion", code: 1, userInfo: [NSLocalizedDescriptionKey: "invalid modifier \(modifier)"])
        }
    }
    return value
}
func send(_ action: Action, pid: pid_t) throws -> [String: Any] {
    if action.kind != "probe" && NSWorkspace.shared.frontmostApplication?.processIdentifier != pid {
        throw NSError(domain: "native-motion", code: 17,
            userInfo: [NSLocalizedDescriptionKey: "target app is not frontmost at native input"])
    }
    switch action.kind {
    case "probe":
        return ["ax": axSnapshot(pid: pid, full: true)]
    case "key":
        guard let keycode = action.keycode, (0...127).contains(keycode) else { throw NSError(domain: "native-motion", code: 2) }
        let source = CGEventSource(stateID: .hidSystemState)
        let down = CGEvent(keyboardEventSource: source, virtualKey: CGKeyCode(keycode), keyDown: true)!
        let up = CGEvent(keyboardEventSource: source, virtualKey: CGKeyCode(keycode), keyDown: false)!
        let modifierFlags = try flags(action.modifiers)
        down.flags = modifierFlags; up.flags = modifierFlags
        down.postToPid(pid); up.postToPid(pid)
        return ["keycode": keycode, "modifiers": action.modifiers ?? []]
    case "move", "click", "click_ax":
        let point: CGPoint
        var targetEvidence: [String: Any] = [:]
        if action.kind == "click_ax" {
            guard let title = action.title else { throw NSError(domain: "native-motion", code: 3) }
            let target = try findAX(pid: pid, title: title, role: action.role)
            guard let rect = axRect(target), let x = rect["x"], let y = rect["y"],
                  let width = rect["width"], let height = rect["height"], width > 0, height > 0 else {
                throw NSError(domain: "native-motion", code: 3,
                    userInfo: [NSLocalizedDescriptionKey: "AX target has no visible bounds"])
            }
            point = CGPoint(x: x + width / 2, y: y + height / 2)
            targetEvidence = ["ax_title": title, "ax_role": action.role ?? "", "ax_bounds_pt": rect]
        } else {
            guard let x = action.x, let y = action.y else { throw NSError(domain: "native-motion", code: 3) }
            point = CGPoint(x: x, y: y) // Global screen points, recorded verbatim.
        }
        let source = CGEventSource(stateID: .hidSystemState)
        let move = CGEvent(mouseEventSource: source, mouseType: .mouseMoved, mouseCursorPosition: point, mouseButton: .left)!
        move.post(tap: .cghidEventTap)
        if action.kind == "click" || action.kind == "click_ax" {
            CGEvent(mouseEventSource: source, mouseType: .leftMouseDown, mouseCursorPosition: point, mouseButton: .left)!.post(tap: .cghidEventTap)
            CGEvent(mouseEventSource: source, mouseType: .leftMouseUp, mouseCursorPosition: point, mouseButton: .left)!.post(tap: .cghidEventTap)
        }
        targetEvidence["global_point_pt"] = ["x": point.x, "y": point.y]
        return targetEvidence
    case "resize":
        guard let width = action.width, let height = action.height, width >= 320, height >= 240 else {
            throw NSError(domain: "native-motion", code: 4)
        }
        let app = AXUIElementCreateApplication(pid)
        guard let window = axElement(app, kAXFocusedWindowAttribute) else {
            throw NSError(domain: "native-motion", code: 5, userInfo: [NSLocalizedDescriptionKey: "no focused AX window"])
        }
        var size = CGSize(width: width, height: height)
        let before = axRect(window) ?? [:]
        guard let value = AXValueCreate(.cgSize, &size),
              AXUIElementSetAttributeValue(window, kAXSizeAttribute as CFString, value) == .success else {
            throw NSError(domain: "native-motion", code: 6, userInfo: [NSLocalizedDescriptionKey: "AX window resize rejected"])
        }
        return ["window_before_pt": before, "requested_size_pt": ["width": width, "height": height],
                "window_after_pt": axRect(window) ?? [:]]
    default: throw NSError(domain: "native-motion", code: 7)
    }
}

@main struct NativeMotion {
    static func main() async {
        do {
            let args = CommandLine.arguments
            if args.count == 3 && args[1] == "list", let pid = Int32(args[2]) {
                let content = try await SCShareableContent.current
                for window in content.windows.filter({ $0.owningApplication?.processID == pid }) {
                    print(String(data: jsonLine(["window_id": window.windowID, "title": window.title ?? "",
                        "visible": window.isOnScreen, "frame_pt": ["x": window.frame.origin.x,
                            "y": window.frame.origin.y, "width": window.frame.width,
                            "height": window.frame.height]]), encoding: .utf8) ?? "{}")
                }
            } else {
                try await run()
            }
        } catch { fputs("native-motion: \(error)\n", stderr); exit(1) }
    }
    static func run() async throws {
        let args = CommandLine.arguments
        guard args.count == 4, let pid = Int32(args[1]) else {
            throw NSError(domain: "native-motion", code: 10, userInfo: [NSLocalizedDescriptionKey: "usage: native-motion-recorder PID PLAN.json OUT"])
        }
        let plan = try JSONDecoder().decode(Plan.self, from: Data(contentsOf: URL(fileURLWithPath: args[2])))
        guard plan.schema == 1, plan.duration_ms > 0, plan.duration_ms <= 30_000,
              plan.max_frames > 0, plan.max_frames <= 1800 else {
            throw NSError(domain: "native-motion", code: 11, userInfo: [NSLocalizedDescriptionKey: "invalid plan bounds"])
        }
        let out = URL(fileURLWithPath: args[3], isDirectory: true)
        let frameDir = out.appendingPathComponent("frames", isDirectory: true)
        try FileManager.default.createDirectory(at: frameDir, withIntermediateDirectories: true)
        let framesURL = out.appendingPathComponent("frames.jsonl")
        FileManager.default.createFile(atPath: framesURL.path, contents: nil)
        let frames = try FileHandle(forWritingTo: framesURL)
        defer { try? frames.close() }
        let actionsURL = out.appendingPathComponent("actions.jsonl")
        FileManager.default.createFile(atPath: actionsURL.path, contents: nil)
        let actions = try FileHandle(forWritingTo: actionsURL)
        defer { try? actions.close() }
        let available = try await SCShareableContent.current
        let candidates = available.windows.filter { $0.owningApplication?.processID == pid && $0.isOnScreen }
        let window: SCWindow
        if let id = plan.window_id {
            guard let selected = candidates.first(where: { $0.windowID == id }) else {
                throw NSError(domain: "native-motion", code: 12, userInfo: [NSLocalizedDescriptionKey: "requested visible PID/window pair not found"])
            }
            window = selected
        } else {
            guard candidates.count == 1, let selected = candidates.first else {
                throw NSError(domain: "native-motion", code: 13, userInfo: [NSLocalizedDescriptionKey: "expected exactly one visible window; specify window_id"])
            }
            window = selected
        }
        let frame = window.frame
        let display = available.displays.max(by: {
            $0.frame.intersection(frame).width * $0.frame.intersection(frame).height <
                $1.frame.intersection(frame).width * $1.frame.intersection(frame).height
        })
        let scope = plan.capture_scope ?? "window"
        let config = SCStreamConfiguration()
        let filter: SCContentFilter
        if scope == "app_display" {
            guard let display else { throw NSError(domain: "native-motion", code: 16,
                userInfo: [NSLocalizedDescriptionKey: "window display not found"]) }
            // Captures dynamically opened app-owned panels and menus too.
            // Other applications are excluded; the full display dimensions
            // remain fixed while the product window resizes.
            let excluded = available.applications.filter { $0.processID != pid }
            filter = SCContentFilter(display: display, excludingApplications: excluded, exceptingWindows: [])
            config.width = display.width
            config.height = display.height
        } else {
            let screens = NSScreen.screens
            let matching = screens.max(by: {
                $0.frame.intersection(frame).width * $0.frame.intersection(frame).height <
                    $1.frame.intersection(frame).width * $1.frame.intersection(frame).height
            })
            let scale = matching?.backingScaleFactor ?? 1
            filter = SCContentFilter(desktopIndependentWindow: window)
            config.width = max(1, Int((frame.width * scale).rounded()))
            config.height = max(1, Int((frame.height * scale).rounded()))
        }
        let fps = plan.capture_fps ?? 30
        config.minimumFrameInterval = CMTime(value: 1, timescale: CMTimeScale(fps))
        config.queueDepth = 3
        config.showsCursor = false // Pointer events are logged, never mistaken for app-pixel motion.
        let recorder = Recorder(frameDir: frameDir, frames: frames, pid: pid, maxFrames: plan.max_frames)
        let stream = SCStream(filter: filter, configuration: config, delegate: recorder)
        try stream.addStreamOutput(recorder, type: .screen, sampleHandlerQueue: DispatchQueue(label: "native-motion.capture"))
        let metadata: [String: Any] = ["pid": pid, "window_id": window.windowID,
            "window_title": window.title ?? "", "window_frame_pt": ["x": frame.origin.x, "y": frame.origin.y, "width": frame.width, "height": frame.height],
            "requested_width_px": config.width, "requested_height_px": config.height,
            "capture_scope": scope, "display_id": display.map { $0.displayID as Any } ?? NSNull(),
            "display_frame_pt": display.map { ["x": $0.frame.origin.x, "y": $0.frame.origin.y,
                "width": $0.frame.width, "height": $0.frame.height] as Any } ?? NSNull(),
            "capture_fps_requested": fps, "frontmost_pid": NSWorkspace.shared.frontmostApplication?.processIdentifier ?? -1,
            "reduce_motion": NSWorkspace.shared.accessibilityDisplayShouldReduceMotion,
            "ax_trusted": AXIsProcessTrusted(), "capture_kind": "native_ScreenCaptureKit_window"]
        try jsonLine(metadata).write(to: out.appendingPathComponent("window.jsonl"))
        try await stream.startCapture()
        var first: UInt64?
        for _ in 0..<100 {
            first = recorder.status().3
            if first != nil { break }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        guard let started = first else {
            try await stream.stopCapture()
            throw NSError(domain: "native-motion", code: 14, userInfo: [NSLocalizedDescriptionKey: "no native window frame arrived"])
        }
        let axSampler = DispatchSource.makeTimerSource(queue: DispatchQueue(label: "native-motion.ax"))
        axSampler.schedule(deadline: .now(), repeating: .milliseconds(50))
        axSampler.setEventHandler { recorder.sampleAX() }
        axSampler.resume()
        defer { axSampler.cancel() }
        actions.write(jsonLine(["phase": "initial", "at_ms": 0, "ax": axSnapshot(pid: pid, full: true)]))
        for action in plan.actions.sorted(by: { $0.at_ms < $1.at_ms }) {
            let target = started + UInt64(action.at_ms) * 1_000_000
            let now = DispatchTime.now().uptimeNanoseconds
            if target > now { try await Task.sleep(nanoseconds: target - now) }
            let before = axSnapshot(pid: pid, full: false)
            let actualMs = Double(DispatchTime.now().uptimeNanoseconds - started) / 1_000_000
            do {
                let delivered = try send(action, pid: pid)
                actions.write(jsonLine(["phase": "action", "label": action.label, "kind": action.kind,
                    "requested_ms": action.at_ms, "actual_ms": actualMs, "before": before, "posted": delivered]))
            } catch {
                actions.write(jsonLine(["phase": "action_failed", "label": action.label, "actual_ms": actualMs, "error": String(describing: error)]))
                throw error
            }
        }
        let end = started + UInt64(plan.duration_ms) * 1_000_000
        let now = DispatchTime.now().uptimeNanoseconds
        if end > now { try await Task.sleep(nanoseconds: end - now) }
        try await stream.stopCapture()
        actions.write(jsonLine(["phase": "final", "at_ms": plan.duration_ms, "ax": axSnapshot(pid: pid, full: true),
            "reduce_motion": NSWorkspace.shared.accessibilityDisplayShouldReduceMotion]))
        let (count, dropped, failure, _) = recorder.status()
        try jsonLine(["captured_frames": count, "dropped_frames": dropped, "stream_failure": failure.map { $0 as Any } ?? NSNull()])
            .write(to: out.appendingPathComponent("result.jsonl"))
        if let failure { throw NSError(domain: "native-motion", code: 15, userInfo: [NSLocalizedDescriptionKey: failure]) }
    }
}
