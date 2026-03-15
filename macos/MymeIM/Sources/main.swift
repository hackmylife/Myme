// main.swift
// Entry point for the MymeIM input method application.
//
// InputMethodKit requires the process to create an IMKServer with the
// connection name from Info.plist, then hand control to NSApp.  The server
// registers this process as the provider of the "MymeIM" input method with
// the macOS input method infrastructure.

import Cocoa
import InputMethodKit

// ---------------------------------------------------------------------------
// Application initialisation (must precede IMKServer creation)
// ---------------------------------------------------------------------------

// Force NSApplication to initialise its shared instance before InputMethodKit
// is touched.  _IMKServerLegacy dereferences properties on the application
// object during server setup; calling this first prevents the null-pointer
// crash at offset 0x8.
let app = NSApplication.shared

// ---------------------------------------------------------------------------
// IMKServer setup
// ---------------------------------------------------------------------------

// The connection name must match the value of "InputMethodConnectionName" in
// Info.plist.  InputMethodKit uses it to look up the controller class from
// the plist key "InputMethodServerControllerClass".
let kConnectionName = "com.myme.inputmethod.Myme_Connection"
let kBundleIdentifier = "com.myme.inputmethod.Myme"

// Retain the server for the lifetime of the process.
// If the server cannot be created (e.g. wrong bundle ID or missing Info.plist
// keys) we still run the event loop so the process does not crash — the user
// would just see no input method activity.
var server: IMKServer? = IMKServer(
    name: kConnectionName,
    bundleIdentifier: kBundleIdentifier
)

if server == nil {
    NSLog("MymeIM: WARNING – IMKServer could not be created.  Check Info.plist.")
} else {
    NSLog("MymeIM: IMKServer started (connection: %@)", kConnectionName)
}

// ---------------------------------------------------------------------------
// Run loop
// ---------------------------------------------------------------------------

// NSApplicationMain-equivalent for a background helper (LSUIElement = YES).
// We must not call NSApplicationMain() because that would look for @main /
// AppDelegate in a way that conflicts with the IMKit lifecycle.
app.run()
