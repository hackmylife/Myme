// MymeInputController.swift
// IMKInputController subclass that wires AppKit/InputMethodKit events
// to the myme Rust engine through MymeBridge.

import Cocoa
import InputMethodKit

@objc(MymeInputController)
class MymeInputController: IMKInputController {

    // -----------------------------------------------------------------------
    // State
    // -----------------------------------------------------------------------

    /// The Rust engine wrapper.  Force-unwrapped after init; guaranteed non-nil
    /// once init succeeds (if init fails we return nil to the caller).
    private var bridge: MymeBridge!

    /// Candidate window provided by InputMethodKit.
    private var candidateWindow: IMKCandidates!

    /// Current candidate list populated by the engine; read by candidates(_:).
    private var currentCandidates: [String] = []

    // -----------------------------------------------------------------------
    // Initialisation
    // -----------------------------------------------------------------------

    override init!(server: IMKServer!, delegate: Any!, client: Any!) {
        // Designated initialiser must call super before touching self.
        super.init(server: server, delegate: delegate, client: client)

        // Locate the SKK dictionary bundled inside the .app.
        let dictPath = MymeInputController.findDictPath()
        let userDictPath = MymeInputController.findUserDictPath()

        // Initialise the bridge with both system and user dictionaries.
        if let b = MymeBridge(dictPath: dictPath, userDictPath: userDictPath) {
            bridge = b
            NSLog("MymeInputController: engine initialised with dict: %@, user: %@",
                  dictPath, userDictPath ?? "(none)")
        } else if let b = MymeBridge(dictPath: dictPath) {
            bridge = b
            NSLog("MymeInputController: engine initialised with dict: %@ (no user dict)", dictPath)
        } else if let b = MymeBridge(dictPath: "") {
            bridge = b
            NSLog("MymeInputController: dict not found; using empty dictionary")
        } else {
            NSLog("MymeInputController: FATAL – could not initialise engine; returning nil")
            return nil
        }

        // Create the floating candidate window once; reuse it across events.
        candidateWindow = IMKCandidates(
            server: server,
            panelType: kIMKSingleColumnScrollingCandidatePanel
        )
    }

    // -----------------------------------------------------------------------
    // Key handling
    // -----------------------------------------------------------------------

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard let event = event else { return false }
        guard let client = sender as? IMKTextInput else { return false }

        // Map the NSEvent to the MymeBridgeKeyType the engine understands.
        guard let bridgeKey = mapEvent(event) else {
            return false
        }

        let action = bridge.handleKey(bridgeKey)

        switch action {
        case .noop:
            return false

        case .updatePreedit(let kana, let pending):
            let preedit = kana + pending
            setMarkedText(preedit, cursorAt: preedit.utf16.count, for: client)
            candidateWindow.hide()
            return true

        case .showCandidates(let segments, _, let candidates, _):
            // Build a multi-segment preedit string with per-segment styling.
            let preedit = buildSegmentPreedit(segments: segments)
            setAttributedMarkedText(preedit, for: client)
            currentCandidates = candidates
            candidateWindow.update()
            candidateWindow.show()
            return true

        case .commit(let text):
            client.setMarkedText(
                "" as NSString,
                selectionRange: NSRange(location: 0, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
            client.insertText(
                text as NSString,
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
            candidateWindow.hide()
            currentCandidates = []
            return true

        case .cancel:
            client.setMarkedText(
                "" as NSString,
                selectionRange: NSRange(location: 0, length: 0),
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
            candidateWindow.hide()
            currentCandidates = []
            return true
        }
    }

    // -----------------------------------------------------------------------
    // IMKCandidates data source
    // -----------------------------------------------------------------------

    override func candidates(_ sender: Any!) -> [Any]! {
        return currentCandidates as [Any]
    }

    override func candidateSelected(_ candidateString: NSAttributedString!) {
        guard let text = candidateString?.string,
              let client = self.client() else { return }

        client.setMarkedText(
            "" as NSString,
            selectionRange: NSRange(location: 0, length: 0),
            replacementRange: NSRange(location: NSNotFound, length: 0)
        )
        client.insertText(
            text as NSString,
            replacementRange: NSRange(location: NSNotFound, length: 0)
        )
        candidateWindow.hide()
        currentCandidates = []
    }

    override func candidateSelectionChanged(_ candidateString: NSAttributedString!) {
        // Nothing to do; the window manages its own highlight.
    }

    // -----------------------------------------------------------------------
    // Session lifecycle
    // -----------------------------------------------------------------------

    override func deactivateServer(_ sender: Any!) {
        let action = bridge.handleKey(.escape)
        if case .commit(let text) = action, !text.isEmpty,
           let client = sender as? IMKTextInput {
            client.insertText(
                text as NSString,
                replacementRange: NSRange(location: NSNotFound, length: 0)
            )
        }
        candidateWindow.hide()
        currentCandidates = []
        super.deactivateServer(sender)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Converts an `NSEvent` into a `MymeBridgeKeyType` the engine can process.
    private func mapEvent(_ event: NSEvent) -> MymeBridgeKeyType? {
        guard event.type == .keyDown else { return nil }

        let modifiers = event.modifierFlags.intersection([.command, .option, .control])
        guard modifiers.isEmpty else { return nil }

        let keyCode = event.keyCode

        switch keyCode {
        case 49:  return .space
        case 36:  return .enter
        case 76:  return .enter
        case 51:  return .backspace
        case 53:  return .escape
        case 126: return .arrowUp
        case 125: return .arrowDown
        case 123: return .arrowLeft
        case 124: return .arrowRight
        default:  break
        }

        guard let chars = event.characters,
              let scalar = chars.unicodeScalars.first else { return nil }

        let value = scalar.value

        if value >= 49 && value <= 57 {
            return .number(UInt8(value - 48))
        }

        guard value >= 0x20 && value <= 0x7E else { return nil }

        return .character(scalar)
    }

    /// Sets in-progress (underlined) text in the current client.
    private func setMarkedText(_ text: String, cursorAt position: Int, for client: IMKTextInput) {
        client.setMarkedText(
            text as NSString,
            selectionRange: NSRange(location: position, length: 0),
            replacementRange: NSRange(location: NSNotFound, length: 0)
        )
    }

    /// Builds an attributed string with per-segment underlines.
    /// Active segment gets a thick underline; others get a thin underline.
    private func buildSegmentPreedit(segments: [MymeBridgeSegment]) -> NSAttributedString {
        let result = NSMutableAttributedString()

        for segment in segments {
            let attrs: [NSAttributedString.Key: Any]
            if segment.isActive {
                attrs = [
                    .underlineStyle: NSUnderlineStyle.thick.rawValue,
                    .underlineColor: NSColor.textColor,
                ]
            } else {
                attrs = [
                    .underlineStyle: NSUnderlineStyle.single.rawValue,
                    .underlineColor: NSColor.secondaryLabelColor,
                ]
            }
            let part = NSAttributedString(string: segment.surface, attributes: attrs)
            result.append(part)
        }

        return result
    }

    /// Sets attributed marked text for multi-segment preedit display.
    private func setAttributedMarkedText(_ text: NSAttributedString, for client: IMKTextInput) {
        let length = text.length
        client.setMarkedText(
            text,
            selectionRange: NSRange(location: length, length: 0),
            replacementRange: NSRange(location: NSNotFound, length: 0)
        )
    }

    /// Returns the path of the bundled SKK dictionary.
    private static func findDictPath() -> String {
        if let bundled = Bundle.main.path(forResource: "system", ofType: "dict") {
            return bundled
        }

        let execURL = URL(fileURLWithPath: Bundle.main.executablePath ?? "")
        let siblingDict = execURL
            .deletingLastPathComponent()
            .appendingPathComponent("system.dict")
        if FileManager.default.fileExists(atPath: siblingDict.path) {
            return siblingDict.path
        }

        let supportDir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/myme")
        let supportDict = supportDir.appendingPathComponent("system.dict")
        if FileManager.default.fileExists(atPath: supportDict.path) {
            return supportDict.path
        }

        NSLog("MymeInputController: no dict found; using empty dictionary")
        return ""
    }

    /// Returns the path to the user dictionary, or nil if not found.
    private static func findUserDictPath() -> String? {
        let supportDir = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/myme")
        let userDict = supportDir.appendingPathComponent("user.dict")
        if FileManager.default.fileExists(atPath: userDict.path) {
            return userDict.path
        }
        return nil
    }
}
