// One black-box instrument for both apps: genuine HID keys -> captured window pixels.
// Build: swiftc -parse-as-library pixels.swift -o pixels
import AppKit
import ScreenCaptureKit
import CoreMedia
import CoreVideo

final class Capture: NSObject, SCStreamOutput, @unchecked Sendable {
    let lock = NSCondition()
    var rect = [0,0,1,1]
    var pixels = [UInt32]()
    var sequence = 0
    var stamp = 0.0
    var width = 0
    var height = 0
    func stream(_ stream: SCStream, didOutputSampleBuffer sample: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen, let b = CMSampleBufferGetImageBuffer(sample) else { return }
        CVPixelBufferLockBaseAddress(b,.readOnly)
        defer { CVPixelBufferUnlockBaseAddress(b,.readOnly) }
        lock.lock(); defer { lock.unlock() }
        width = CVPixelBufferGetWidth(b); height = CVPixelBufferGetHeight(b)
        let stride = CVPixelBufferGetBytesPerRow(b)/4
        let src = CVPixelBufferGetBaseAddress(b)!.assumingMemoryBound(to:UInt32.self)
        var out = [UInt32](); out.reserveCapacity(rect[2]*rect[3])
        for y in max(0,rect[1])..<min(height,rect[1]+rect[3]) {
            for x in max(0,rect[0])..<min(width,rect[0]+rect[2]) { out.append(src[y*stride+x] & 0xffffff) }
        }
        pixels = out; sequence += 1; stamp = ProcessInfo.processInfo.systemUptime
        lock.broadcast()
    }
    func frame(after:Int = -1,timeout:Double = 3) -> ([UInt32],Int,Double) {
        lock.lock(); defer { lock.unlock() }
        let end=Date().addingTimeInterval(timeout)
        while sequence <= after { if !lock.wait(until:end) { break } }
        return(pixels,sequence,stamp)
    }
    func region(_ r:[Int]) -> ([UInt32],Int,Double) {
        lock.lock(); rect=r; let s=sequence; lock.unlock()
        return frame(after:s)
    }
}
func emit(_ obj:[String:Any]) { let d=try! JSONSerialization.data(withJSONObject:obj,options:[.sortedKeys]); print(String(data:d,encoding:.utf8)!); fflush(stdout) }
func key(_ code:Int,_ flags:UInt64=0) {
    for down in [true,false] { let e=CGEvent(keyboardEventSource:nil,virtualKey:CGKeyCode(code),keyDown:down)!; e.flags=CGEventFlags(rawValue:flags); e.post(tap:.cghidEventTap) }
}
@main struct Main {
 static func main() async throws {
    _ = NSApplication.shared
    if CommandLine.arguments.contains("--request-permissions") {
        _ = AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary)
        if !CGPreflightScreenCaptureAccess() { _ = CGRequestScreenCaptureAccess() }
        for _ in 0..<240 {
            let ax=AXIsProcessTrusted(); let screen=CGPreflightScreenCaptureAccess()
            if ax && screen { emit(["permissions_ready":true]); return }
            try await Task.sleep(nanoseconds:500_000_000)
        }
        emit(["accessibility":AXIsProcessTrusted(),"screen_recording":CGPreflightScreenCaptureAccess()]); return
    }
    if CommandLine.arguments.count > 4 { freopen(CommandLine.arguments[3], "r", stdin); freopen(CommandLine.arguments[4], "w", stdout) }

    let pid=pid_t(CommandLine.arguments[1])!; let wid=CGWindowID(CommandLine.arguments[2])!
    guard AXIsProcessTrusted() else { emit(["error":"Accessibility denied to instrument"]); return }
    guard CGPreflightScreenCaptureAccess() else { emit(["error":"Screen Recording denied to instrument; run --request-permissions once"]); return }
    let content=try await SCShareableContent.excludingDesktopWindows(false,onScreenWindowsOnly:false)
    guard let window=content.windows.first(where:{$0.windowID==wid && $0.owningApplication?.processID==pid}) else { emit(["error":"exact window missing"]); return }
    let conf=SCStreamConfiguration(); conf.width=Int(window.frame.width*2); conf.height=Int(window.frame.height*2)
    conf.pixelFormat=kCVPixelFormatType_32BGRA; conf.minimumFrameInterval=CMTime(value:1,timescale:120); conf.queueDepth=3; conf.showsCursor=false; conf.capturesAudio=false
    let capture=Capture(); let stream=SCStream(filter:SCContentFilter(desktopIndependentWindow:window),configuration:conf,delegate:nil)
    try stream.addStreamOutput(capture,type:.screen,sampleHandlerQueue:DispatchQueue(label:"s11.capture"))
    try await stream.startCapture()
    emit(["ready":true,"width":conf.width,"height":conf.height,"requested_fps":120])
    await Task.detached {
      while let line=readLine() {
        do {
          let args=try JSONSerialization.jsonObject(with:Data(line.utf8)) as! [String:Any]
          let op=args["op"] as! String
          if op=="quit" { break }
          if op=="region" { let f=capture.region(args["rect"] as! [Int]); emit(["pixels":f.0,"seq":f.1]); continue }
          if op=="frame" { let f=capture.frame(); emit(["pixels":f.0,"seq":f.1]); continue }
          guard NSWorkspace.shared.frontmostApplication?.processIdentifier==pid else { emit(["error":"focus lost; refusing input"]); continue }
          let baseline=capture.region(args["rect"] as! [Int]); let t0=ProcessInfo.processInfo.systemUptime
          if op=="key" { key(args["code"] as! Int,(args["flags"] as? NSNumber)?.uint64Value ?? 0) }
          else if op=="click" {
            let p=CGPoint(x:args["x"] as! Double,y:args["y"] as! Double)
            for type in [CGEventType.leftMouseDown,.leftMouseUp] { CGEvent(mouseEventSource:nil,mouseType:type,mouseCursorPosition:p,mouseButton:.left)!.post(tap:.cghidEventTap) }
          }
          else if op=="resize" {
            let app=AXUIElementCreateApplication(pid); var value:CFTypeRef?
            AXUIElementCopyAttributeValue(app,kAXFocusedWindowAttribute as CFString,&value)
            guard let value else { emit(["error":"no focused AX window"]); continue }
            var size=CGSize(width:args["width"] as! Double,height:args["height"] as! Double)
            let e=AXUIElementSetAttributeValue(value as! AXUIElement,kAXSizeAttribute as CFString,AXValueCreate(.cgSize,&size)!)
            if e != .success { emit(["error":"resize refused \(e.rawValue)"]); continue }
          } else { emit(["error":"unknown operation"]); continue }
          var seq=baseline.1; var last=baseline.0; var found:Double?; var rejected=0
          let timeout=args["timeout"] as? Double ?? 5
          while ProcessInfo.processInfo.systemUptime-t0 < timeout {
            let f=capture.frame(after:seq,timeout:0.1); seq=f.1
            if f.0 != last {
              let colors=Set(zip(last,f.0).compactMap { $0 != $1 ? $1 : nil })
              if !(args["echo"] as? Bool ?? false) || colors.count>=3 { found=f.2-t0; break }
              rejected+=1; last=f.0
            }
          }
          emit(["ms":found.map{$0*1000} as Any? ?? NSNull(),"timeout":found==nil,"ignored_blinks":rejected])
        } catch { emit(["error":String(describing:error)]) }
      }
    }.value
    try await stream.stopCapture()
 }
}
