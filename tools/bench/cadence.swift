// What actually reaches the screen while Puzil scrolls, frame by frame.
//
//   swiftc -O tools/bench/cadence.swift -o dist/cadence
//   PUZIL_BENCH=… target/release/puzil-app &   # then, while it runs:
//   dist/cadence PID SECONDS OUT.json [DUMP=dir saves every 200th frame]
//
// Captures the largest window of PID with ScreenCaptureKit at one frame per display refresh
// and, for each frame, records its timestamp, whether its content changed ("idle" frames
// didn't), and how far the middle of the window moved vertically since the last new frame.
// While the benchmark scrolls 40 points per frame, a smooth run moves 80 pixels (2× display)
// on every refresh; 0 is a repeated frame, 160 a skipped one. The capture itself samples a
// little slower than the display, so expect ~5% skips from it alone. Needs Screen Recording
// permission for the terminal that runs it.
import Foundation
import ScreenCaptureKit
import CoreMedia
import CoreVideo
import AppKit
import CoreImage
import ImageIO
import UniformTypeIdentifiers

struct Rec: Codable { let t: Double; let status: String; let shift: Int?; let err: Double? }

final class Capture: NSObject, SCStreamOutput {
    var recs: [Rec] = []
    var prev: [Double]? = nil
    let lock = NSLock()

    // Mean brightness of each row over the middle band of columns, for rows in the middle
    // of the window (clear of the header and the composer).
    func signature(_ pb: CVPixelBuffer) -> [Double] {
        CVPixelBufferLockBaseAddress(pb, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pb, .readOnly) }
        let w = CVPixelBufferGetWidth(pb), h = CVPixelBufferGetHeight(pb)
        let bpr = CVPixelBufferGetBytesPerRow(pb)
        let base = CVPixelBufferGetBaseAddress(pb)!.assumingMemoryBound(to: UInt8.self)
        let x0 = w * 35 / 100, x1 = w * 95 / 100
        let y0 = h * 15 / 100, y1 = h * 80 / 100
        var sig = [Double](repeating: 0, count: y1 - y0)
        for y in y0..<y1 {
            var s = 0
            let row = base + y * bpr
            var x = x0
            while x < x1 { s += Int(row[x * 4]) + Int(row[x * 4 + 1]) + Int(row[x * 4 + 2]); x += 2 }
            sig[y - y0] = Double(s) / Double((x1 - x0) / 2 * 3)
        }
        return sig
    }

    // The vertical offset that best aligns b onto a (content moved down by `d` rows means
    // b[i] == a[i - d]), by mean absolute difference over the overlap.
    func shift(_ a: [Double], _ b: [Double]) -> (Int, Double) {
        var best = (0, Double.infinity)
        let n = a.count
        for d in -240...240 {
            var s = 0.0, c = 0
            var i = max(0, d)
            while i < min(n, n + d) { s += abs(b[i] - a[i - d]); c += 1; i += 1 }
            if c > n / 3 { let e = s / Double(c); if e < best.1 { best = (d, e) } }
        }
        return best
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sb: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen else { return }
        let t = CMSampleBufferGetPresentationTimeStamp(sb).seconds
        var status = "unknown"
        if let atts = CMSampleBufferGetSampleAttachmentsArray(sb, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
           let raw = atts.first?[.status] as? Int, let st = SCFrameStatus(rawValue: raw) {
            switch st {
            case .complete: status = "complete"
            case .idle: status = "idle"
            case .blank: status = "blank"
            case .suspended: status = "suspended"
            case .started: status = "started"
            case .stopped: status = "stopped"
            @unknown default: status = "other"
            }
        }
        var rec = Rec(t: t, status: status, shift: nil, err: nil)
        if status == "complete", let pb = CMSampleBufferGetImageBuffer(sb), let dir = ProcessInfo.processInfo.environment["DUMP"], recs.count % 200 == 0 {
            let ci = CIImage(cvPixelBuffer: pb)
            if let cg = CIContext().createCGImage(ci, from: ci.extent),
               let dst = CGImageDestinationCreateWithURL(URL(fileURLWithPath: "\(dir)/f\(recs.count).png") as CFURL, UTType.png.identifier as CFString, 1, nil) {
                CGImageDestinationAddImage(dst, cg, nil); CGImageDestinationFinalize(dst)
            }
        }
        if status == "complete", let pb = CMSampleBufferGetImageBuffer(sb) {
            let sig = signature(pb)
            if let p = prev { let (d, e) = shift(p, sig); rec = Rec(t: t, status: status, shift: d, err: e) }
            prev = sig
        }
        lock.lock(); recs.append(rec); lock.unlock()
    }
}

let args = CommandLine.arguments
let pid = pid_t(args[1])!, seconds = Double(args[2])!, out = args[3]
_ = NSApplication.shared
let cap = Capture()
let sema = DispatchSemaphore(value: 0)
Task {
    do {
        var win: SCWindow? = nil
        for _ in 0..<50 {
            let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
            win = content.windows.filter { $0.owningApplication?.processID == pid }.max { $0.frame.width * $0.frame.height < $1.frame.width * $1.frame.height }
            if win != nil { break }
            try await Task.sleep(nanoseconds: 100_000_000)
        }
        guard let w = win else { print("no window for pid"); exit(2) }
        let cfg = SCStreamConfiguration()
        cfg.width = Int(w.frame.width * 2); cfg.height = Int(w.frame.height * 2)
        cfg.minimumFrameInterval = CMTime(value: 1, timescale: 60)
        cfg.pixelFormat = kCVPixelFormatType_32BGRA
        cfg.showsCursor = false
        cfg.queueDepth = 8
        let stream = SCStream(filter: SCContentFilter(desktopIndependentWindow: w), configuration: cfg, delegate: nil)
        try stream.addStreamOutput(cap, type: .screen, sampleHandlerQueue: DispatchQueue(label: "cap"))
        try await stream.startCapture()
        try await Task.sleep(nanoseconds: UInt64(seconds * 1e9))
        try? await stream.stopCapture()
        cap.lock.lock()
        let data = try JSONEncoder().encode(cap.recs)
        cap.lock.unlock()
        try data.write(to: URL(fileURLWithPath: out))
        print("captured \(cap.recs.count) frames, window \(Int(w.frame.width))x\(Int(w.frame.height))")
    } catch { print("capture failed: \(error)"); exit(1) }
    sema.signal()
}
sema.wait()
