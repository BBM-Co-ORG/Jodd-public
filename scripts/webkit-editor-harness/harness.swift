// Drives Jodd's editor wiring inside a real WKWebView — the engine Jodd runs on
// macOS — with genuine keyboard events, and prints the editor's HTML and caret
// after each step. See README.md for why it exists and how run.sh uses it.
//
// Events are NSEvents queued on this app's own event queue (NSApp.postEvent),
// not CGEvent.postToPid (which never reaches an inactive app) and not JS
// `execCommand` calls (which never close an NSUndoManager group, so every
// edit collapses into one undo step and the measurement is fiction). NSApp.run
// dequeues each event in its own pass, so undo groups per keystroke exactly as
// in the app. A local event monitor routes them the way AppKit routes a key
// window's: Cmd-chords to WKWebView.performKeyEquivalent first, then to an
// Edit menu mirroring Tauri 2's default (Undo ⌘Z / Redo ⇧⌘Z → undo:/redo:);
// everything else through sendEvent to the first responder.
//
// Usage: harness <page.html> <keys.txt>
//   keys.txt lines:  type <text> | key <combo> | snap <label> | js <code> | wait <ms>
//   combos:          [cmd+][shift+][ctrl+][alt+]<z|y|a|e|b|k|enter|backspace|space|tab>
import Cocoa
import WebKit

let args = CommandLine.arguments
let pageHTML = try! String(contentsOfFile: args[1], encoding: .utf8)
let keyLines = try! String(contentsOfFile: args[2], encoding: .utf8)
    .split(separator: "\n").map(String.init).filter { !$0.trimmingCharacters(in: .whitespaces).isEmpty }

let app = NSApplication.shared
app.setActivationPolicy(.accessory)

// Tauri 2.11.5's default Edit menu: PredefinedMenuItem::undo/redo → undo:/redo:, nil target.
let mainMenu = NSMenu()
let editItem = NSMenuItem(title: "Edit", action: nil, keyEquivalent: "")
let edit = NSMenu(title: "Edit")
let undoItem = NSMenuItem(title: "Undo", action: Selector(("undo:")), keyEquivalent: "z")
undoItem.keyEquivalentModifierMask = [.command]
let redoItem = NSMenuItem(title: "Redo", action: Selector(("redo:")), keyEquivalent: "Z")
redoItem.keyEquivalentModifierMask = [.command, .shift]
edit.addItem(undoItem); edit.addItem(redoItem)
editItem.submenu = edit; mainMenu.addItem(editItem); app.mainMenu = mainMenu

let win = NSWindow(contentRect: NSRect(x: -3000, y: -3000, width: 700, height: 500),
                   styleMask: [.titled], backing: .buffered, defer: false)
let web = WKWebView(frame: win.contentView!.bounds)
win.contentView!.addSubview(web)
win.orderFront(nil)
win.makeFirstResponder(web)

var menuHits: [String] = []
_ = NSEvent.addLocalMonitorForEvents(matching: [.keyDown, .keyUp]) { ev in
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

let codes: [String: CGKeyCode] = ["z": 6, "enter": 36, "backspace": 51, "space": 49, "tab": 48, "y": 16, "a": 0, "e": 14, "b": 11, "k": 40, "9": 25]
// Queue genuine NSEvents on the app's own event queue: NSApp.run dequeues each
// one in its own pass (so NSUndoManager groups per keystroke) and dispatches it
// through sendEvent → the local monitor above.
func post(code: CGKeyCode, flags: CGEventFlags, text: String?) {
    var mods: NSEvent.ModifierFlags = []
    if flags.contains(.maskCommand) { mods.insert(.command) }
    if flags.contains(.maskShift) { mods.insert(.shift) }
    if flags.contains(.maskControl) { mods.insert(.control) }
    if flags.contains(.maskAlternate) { mods.insert(.option) }
    let chars = text ?? ""
    let ignoring = chars.lowercased()
    for type in [NSEvent.EventType.keyDown, .keyUp] {
        let ev = NSEvent.keyEvent(with: type, location: .zero, modifierFlags: mods,
                                  timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: win.windowNumber,
                                  context: nil, characters: chars, charactersIgnoringModifiers: ignoring,
                                  isARepeat: false, keyCode: UInt16(code))!
        NSApp.postEvent(ev, atStart: false)
    }
}

var i = 0
func next() {
    guard i < keyLines.count else {
        web.evaluateJavaScript("window.snapshot()") { _, _ in exit(0) }
        return
    }
    let line = keyLines[i]; i += 1
    let parts = line.split(separator: " ", maxSplits: 1).map(String.init)
    let cmd = parts[0]; let rest = parts.count > 1 ? parts[1] : ""
    switch cmd {
    case "type":
        var chars = Array(rest)
        func one() {
            guard !chars.isEmpty else { DispatchQueue.main.asyncAfter(deadline: .now() + 0.15, execute: next); return }
            let c = chars.removeFirst()
            post(code: c == " " ? 49 : 0, flags: [], text: String(c))
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.08, execute: one)
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
        post(code: codes[k]!, flags: flags, text: text)
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: next)
    case "snap":
        web.evaluateJavaScript("window.snapshot()") { v, e in
            let hits = menuHits.isEmpty ? "" : "   MENU=\(menuHits)"
            menuHits = []
            print(rest.padding(toLength: 26, withPad: " ", startingAt: 0) + " | " + "\(v ?? e.map { "ERR \($0)" } ?? "nil")" + hits)
            next()
        }
    case "js":
        web.evaluateJavaScript(rest) { v, e in if let e = e { print("JS ERR", e) }; next() }
    case "wait":
        DispatchQueue.main.asyncAfter(deadline: .now() + (Double(rest) ?? 100) / 1000, execute: next)
    default:
        print("bad line: \(line)"); next()
    }
}

final class Nav: NSObject, WKNavigationDelegate {
    func webView(_ w: WKWebView, didFinish n: WKNavigation!) {
        win.makeFirstResponder(web)
        w.evaluateJavaScript("window.setup && window.setup(); 0") { _, e in
            if let e = e { print("setup ERR", e) }
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.3, execute: next)
        }
    }
}
let nav = Nav()
web.navigationDelegate = nav
web.loadHTMLString(pageHTML, baseURL: nil)
DispatchQueue.main.asyncAfter(deadline: .now() + 60) { print("TIMEOUT"); exit(1) }
app.run()
