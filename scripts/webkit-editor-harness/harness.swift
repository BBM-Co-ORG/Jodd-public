// Drives Jodd's editor wiring inside a real WKWebView — the engine Jodd runs on
// macOS — with genuine keyboard events, and prints the editor's HTML and caret
// after each step. See README.md for why it exists and how run.sh uses it.
//
// Events are NSEvents queued on this app's own event queue (NSApp.postEvent),
// not CGEvent.postToPid (which never reaches an inactive app) and not JS
// `execCommand` calls (which never close an NSUndoManager group, so every
// edit collapses into one undo step and the measurement is fiction). NSApp.run
// (tao runs the same loop in the app) ends the open NSUndoManager group when it
// dequeues the next event — ANY event — so where each event lands decides the
// undo steps; press() and waitForStartupEvents() pin that down (README: flaky
// diffs). A local event monitor routes them the way AppKit routes a key
// window's: Cmd-chords to WKWebView.performKeyEquivalent first, then to an
// Edit menu mirroring Tauri 2's default (Undo ⌘Z / Redo ⇧⌘Z → undo:/redo:);
// everything else through sendEvent to the first responder.
//
// Usage: harness <page.html> <keys.txt>
//   keys.txt lines:  type <text> | key <combo> | snap <label> | js <code> | wait <ms>
//                    | clip <text> | cliphtml <html>
//   combos:          [cmd+][shift+][ctrl+][alt+]<z|y|a|e|b|k|v|enter|backspace|space|tab>
//   clip / cliphtml: (in clip, the two characters \n become a newline) put text (or text + HTML, as a copy from a web page does) on the
//                    system pasteboard for the next Cmd+V. The user's own clipboard
//                    text is saved on first use and put back when the harness exits.
import Cocoa
import WebKit

let args = CommandLine.arguments
let pageHTML = try! String(contentsOfFile: args[1], encoding: .utf8)
let keyLines = try! String(contentsOfFile: args[2], encoding: .utf8)
    .split(separator: "\n").map(String.init).filter { !$0.trimmingCharacters(in: .whitespaces).isEmpty }

let app = NSApplication.shared
atexit { restoreClip() }
app.setActivationPolicy(.accessory)

// Tauri 2.11.5's default Edit menu: PredefinedMenuItem::undo/redo → undo:/redo:, nil target.
let mainMenu = NSMenu()
let editItem = NSMenuItem(title: "Edit", action: nil, keyEquivalent: "")
let edit = NSMenu(title: "Edit")
let undoItem = NSMenuItem(title: "Undo", action: Selector(("undo:")), keyEquivalent: "z")
undoItem.keyEquivalentModifierMask = [.command]
let redoItem = NSMenuItem(title: "Redo", action: Selector(("redo:")), keyEquivalent: "Z")
redoItem.keyEquivalentModifierMask = [.command, .shift]
// Cut/Copy/Paste too, as Tauri's default Edit menu has them (measured: the Edit menu of
// the installed app lists Undo, Redo, Cut, Copy, Paste, Select All). Without Paste, Cmd+V
// reaches nothing and the page never gets a paste event.
let pasteItem = NSMenuItem(title: "Paste", action: Selector(("paste:")), keyEquivalent: "v")
pasteItem.keyEquivalentModifierMask = [.command]
edit.addItem(undoItem); edit.addItem(redoItem); edit.addItem(pasteItem)
editItem.submenu = edit; mainMenu.addItem(editItem); app.mainMenu = mainMenu

// On screen, not off it: AppKit put the old (-3000, -3000) window back in the
// middle of the display anyway (measured), and WebKit needs the window shown
// to run requestAnimationFrame (see settle()).
// Transparent to the mouse: a WKWebView gets mouseMoved at 60 Hz while the
// cursor crosses it, even in an inactive app, and NSUndoManager ends the
// open undo group on EVERY event it dequeues — moving the mouse over the
// window split undo steps at random (README: flaky diffs).
let screen = NSScreen.main!.visibleFrame
let win = NSWindow(contentRect: NSRect(x: screen.minX, y: screen.minY, width: 700, height: 500),
                   styleMask: [.titled], backing: .buffered, defer: false)
win.ignoresMouseEvents = true
let web = WKWebView(frame: win.contentView!.bounds)
win.contentView!.addSubview(web)
win.orderFront(nil)
win.makeFirstResponder(web)

// HARNESS_TRACE=<file>: log each key's dequeue and each NSUndoManager group
// open/close and every other event, with ms timestamps — how the flaky diffs
// were traced (README).
let traceFH = ProcessInfo.processInfo.environment["HARNESS_TRACE"].flatMap { FileManager.default.createFile(atPath: $0, contents: nil) ? FileHandle(forWritingAtPath: $0) : nil }
let t0 = ProcessInfo.processInfo.systemUptime
var step = "setup"
func trace(_ s: String) {
    traceFH?.write((String(format: "%8.1f ", (ProcessInfo.processInfo.systemUptime - t0) * 1000) + "[\(step)] \(s)\n").data(using: .utf8)!)
}
var undoObservers: [Any] = []
func traceUndo(_ um: UndoManager) {
    guard traceFH != nil else { return }
    for (name, label) in [(Notification.Name.NSUndoManagerDidOpenUndoGroup, "undo group OPEN"), (.NSUndoManagerDidCloseUndoGroup, "undo group CLOSE"),
                          (.NSUndoManagerDidUndoChange, "undid"), (.NSUndoManagerDidRedoChange, "redid")] {
        undoObservers.append(NotificationCenter.default.addObserver(forName: name, object: um, queue: nil) { _ in trace(label) })
    }
}

var menuHits: [String] = []
// Set by press(): runs once the keyDown it posted has been dispatched.
var afterKeyDown: (() -> Void)?
_ = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp]) { ev in
    trace("key \(ev.type == .keyDown ? "down" : "up") \(ev.keyCode)")
    defer {
        if ev.type == .keyDown, let k = afterKeyDown { afterKeyDown = nil; DispatchQueue.main.async(execute: k) }
    }
    if ev.type == .keyDown && ev.modifierFlags.contains(.command) {
        if web.performKeyEquivalent(with: ev) { return nil }
        if let item = mainMenu.items.first?.submenu?.items.first(where: {
            $0.keyEquivalent == ev.charactersIgnoringModifiers?.lowercased()
                && $0.keyEquivalentModifierMask == ev.modifierFlags.intersection([.command, .shift, .control, .option])
                || ($0.keyEquivalent == "Z" && ev.modifierFlags.intersection([.command, .shift, .control, .option]) == [.command, .shift] && ev.charactersIgnoringModifiers?.lowercased() == "z")
        }) {
            menuHits.append(item.title)
            NSApp.sendAction(item.action!, to: nil, from: item)
            return nil
        }
        return nil
    }
    win.sendEvent(ev)
    return nil
}
// Every other event ends the open undo group too — dispatched or swallowed,
// so a monitor cannot neutralise one. The window sends itself twelve
// appKitDefined events while it comes up, in two bursts, the second landing
// within ~70 ms of the second windowMoved (subtype 4) and 300-800 ms after the
// first; under load it slid into the first steps and split their undo step.
// waitForStartupEvents() lets them pass; any event after that which ends an
// open group is printed, so a disturbed run fails and says why.
var windowMoves = 0
var lastOtherEvent = ProcessInfo.processInfo.systemUptime
var stepsStarted = false
_ = NSEvent.addLocalMonitorForEvents(matching: NSEvent.EventTypeMask.any.subtracting([.keyDown, .keyUp])) { ev in
    let desc = "type=\(ev.type.rawValue)" + (ev.type == .appKitDefined ? " subtype=\(ev.subtype.rawValue)" : "")
    trace("other event " + desc)
    lastOtherEvent = ProcessInfo.processInfo.systemUptime
    if ev.type == .appKitDefined && ev.subtype == .windowMoved { windowMoves += 1 }
    if stepsStarted, let um = web.undoManager, um.groupingLevel > 0 {
        print("STRAY EVENT \(desc) ended an open undo step during: \(step)")
    }
    return ev
}
func waitForStartupEvents(deadline: TimeInterval = ProcessInfo.processInfo.systemUptime + 3, _ then: @escaping () -> Void) {
    let now = ProcessInfo.processInfo.systemUptime
    if (windowMoves >= 2 && now - lastOtherEvent >= 0.3) || now >= deadline {
        stepsStarted = true
        then()
    } else {
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) { waitForStartupEvents(deadline: deadline, then) }
    }
}

let codes: [String: CGKeyCode] = ["z": 6, "enter": 36, "backspace": 51, "space": 49, "tab": 48, "y": 16, "a": 0, "e": 14, "b": 11, "k": 40, "9": 25, "v": 9]
// Resolves once the page has handled everything sent to it so far and one
// animation frame has run (the markdown triggers apply in rAF). The reply
// travels on the same ordered connection as the undo registrations those
// edits sent, so when it lands they have all reached NSUndoManager.
// If no frame comes within 2 s WebKit has stopped rendering the window —
// the display slept, or another harness window covers this one (measured:
// two concurrent runs, rAF never ran in one of them) — and every markdown
// trigger would silently not fire, so stop rather than record that.
func settle(_ then: @escaping () -> Void) {
    web.callAsyncJavaScript("""
        return await new Promise(r => { requestAnimationFrame(() => setTimeout(() => r('frame'), 0)); setTimeout(() => r('no frame'), 2000); })
        """, arguments: [:], in: nil, in: .page) { result in
        if case .success(let v) = result, v as? String == "frame" { then(); return }
        print("requestAnimationFrame stopped running during: \(step) — is the display asleep, or another harness running?")
        exit(2)
    }
}

// Presses one key with genuine NSEvents queued on the app's own event queue:
// NSApp.run dequeues each in its own pass and dispatches it through sendEvent
// → the local monitor above. NSUndoManager closes its open group when the
// NEXT event is dequeued, so the keyUp is what ends this keystroke's undo
// step — and WebKit's registrations arrive over IPC, after the keyDown.
// The keyUp is therefore held back until the page has settled, the way a
// real key is held for ~100 ms. Queued straight behind the keyDown it was
// dequeued ~1 ms before the registrations, and under load sometimes between
// two of them, splitting one undo step in two (README: flaky diffs).
func press(code: CGKeyCode, flags: CGEventFlags, text: String?, then: @escaping () -> Void) {
    var mods: NSEvent.ModifierFlags = []
    if flags.contains(.maskCommand) { mods.insert(.command) }
    if flags.contains(.maskShift) { mods.insert(.shift) }
    if flags.contains(.maskControl) { mods.insert(.control) }
    if flags.contains(.maskAlternate) { mods.insert(.option) }
    let chars = text ?? ""
    let ignoring = chars.lowercased()
    func event(_ type: NSEvent.EventType) -> NSEvent {
        NSEvent.keyEvent(with: type, location: .zero, modifierFlags: mods,
                         timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: win.windowNumber,
                         context: nil, characters: chars, charactersIgnoringModifiers: ignoring,
                         isARepeat: false, keyCode: UInt16(code))!
    }
    afterKeyDown = {
        settle {
            NSApp.postEvent(event(.keyUp), atStart: false)
            then()
        }
    }
    NSApp.postEvent(event(.keyDown), atStart: false)
}

// clip / cliphtml write the GENERAL pasteboard, because that is what WKWebView's
// paste: reads — so the user's clipboard is borrowed, and restored at exit.
var savedClip: String?? = .none
func borrowClip() {
    if savedClip == nil { savedClip = .some(NSPasteboard.general.string(forType: .string)) }
}
func restoreClip() {
    guard let saved = savedClip else { return }
    NSPasteboard.general.clearContents()
    if let s = saved { NSPasteboard.general.setString(s, forType: .string) }
}

var i = 0
func next() {
    guard i < keyLines.count else {
        web.evaluateJavaScript("window.snapshot()") { _, _ in exit(0) }
        return
    }
    let line = keyLines[i]; i += 1
    step = line
    let parts = line.split(separator: " ", maxSplits: 1).map(String.init)
    let cmd = parts[0]; let rest = parts.count > 1 ? parts[1] : ""
    switch cmd {
    case "type":
        var chars = Array(rest)
        func one() {
            guard !chars.isEmpty else { DispatchQueue.main.asyncAfter(deadline: .now() + 0.15, execute: next); return }
            let c = chars.removeFirst()
            press(code: c == " " ? 49 : 0, flags: [], text: String(c)) {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.08, execute: one)
            }
        }
        one()
    case "key":
        var flags: CGEventFlags = []
        let toks = rest.split(separator: "+").map(String.init)
        for t in toks.dropLast() {
            if t == "cmd" { flags.insert(.maskCommand) }
            if t == "shift" { flags.insert(.maskShift) }
            if t == "ctrl" { flags.insert(.maskControl) }
            if t == "alt" { flags.insert(.maskAlternate) }
        }
        let k = toks.last!
        let text: String? = k.count == 1 ? (flags.contains(.maskShift) ? k.uppercased() : k) : (k == "enter" ? "\r" : k == "backspace" ? "\u{7f}" : k == "tab" ? "\t" : k == "space" ? " " : nil)
        press(code: codes[k]!, flags: flags, text: text) {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: next)
        }
    case "snap":
        web.evaluateJavaScript("window.snapshot()") { v, e in
            let hits = menuHits.isEmpty ? "" : "   MENU=\(menuHits)"
            menuHits = []
            print(rest.padding(toLength: 26, withPad: " ", startingAt: 0) + " | " + "\(v ?? e.map { "ERR \($0)" } ?? "nil")" + hits)
            next()
        }
    case "js":
        web.evaluateJavaScript(rest) { v, e in if let e = e { print("JS ERR", e) }; next() }
    case "clip":
        borrowClip()
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(rest.replacingOccurrences(of: "\\n", with: "\n"), forType: .string)
        next()
    case "cliphtml":
        borrowClip()
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(rest, forType: .html)
        NSPasteboard.general.setString(rest.replacingOccurrences(of: "<[^>]*>", with: "", options: .regularExpression), forType: .string)
        next()
    case "wait":
        DispatchQueue.main.asyncAfter(deadline: .now() + (Double(rest) ?? 100) / 1000, execute: next)
    default:
        print("bad line: \(line)"); next()
    }
}

final class Nav: NSObject, WKNavigationDelegate {
    func webView(_ w: WKWebView, didFinish n: WKNavigation!) {
        win.makeFirstResponder(web)
        if let um = web.undoManager { traceUndo(um) }
        w.evaluateJavaScript("window.setup && window.setup(); 0") { _, e in
            if let e = e { print("setup ERR", e) }
            waitForStartupEvents(next)
        }
    }
}
let nav = Nav()
web.navigationDelegate = nav
web.loadHTMLString(pageHTML, baseURL: nil)
DispatchQueue.main.asyncAfter(deadline: .now() + 60) { print("TIMEOUT"); exit(1) }
app.run()
