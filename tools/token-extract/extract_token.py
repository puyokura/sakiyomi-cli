#!/usr/bin/env python3
"""Read the saKIYOMI session token out of a running Android app.

The app keeps its token in Expo's SecureStore, AES-wrapped with a key held in
AndroidKeyStore. The key material is not recoverable from a backup, but it does
not have to be: the module necessarily returns the plaintext to JavaScript
whenever it reads the token, and that call is hookable.

Verified on a rooted MuMu emulator (Android 14, x86_64) with the released app
(`app.sakiyomi.mobile` 1.0.42):

* frida-server on the device **must** match the frida client version on the host.
  With mismatched versions the Java bridge is unavailable (`typeof Java` is
  `undefined`) and every hook silently fails.
* The hook must be installed **before** the app starts. The app decrypts the
  token during startup, so attaching afterwards sees nothing.
* The value is a 64-character hex string and is the same on every run.

Usage:
    python extract_token.py --out token.txt [--device 127.0.0.1:27042]
                            [--package app.sakiyomi.mobile]

The token is written to the file named by --out and never printed.

Requirements:
    pip install "frida==<the version your device's frida-server reports>"
    adb shell /data/local/tmp/frida-server &      # or however you run it
    adb forward tcp:27042 tcp:27042
"""

from __future__ import annotations

import argparse
import subprocess
import sys
import time

PACKAGE = "app.sakiyomi.mobile"

# Hook the one place the plaintext exists: the decryptor's return value.
# AESEncryptor.decryptItem is called by SecureStoreModule.getItemImpl, so hooking
# either is enough; both are hooked in case the app's flow skips one.
AGENT = r"""
Java.perform(function () {
    var reported = false;

    function report(value) {
        if (reported || value === null || value === undefined) return;
        var s = String(value).trim();
        if (s.length < 32) return;
        reported = true;
        send({ kind: "token", value: s });
    }

    function hookDecryptor() {
        var A = Java.use("expo.modules.securestore.encryptors.AESEncryptor");
        A.decryptItem.overloads.forEach(function (overload) {
            overload.implementation = function () {
                var result = overload.apply(this, arguments);
                report(result);
                return result;
            };
        });
        send({ kind: "hooked", value: "AESEncryptor.decryptItem" });
    }

    function hookModule() {
        var M = Java.use("expo.modules.securestore.SecureStoreModule");
        M.getItemImpl.overloads.forEach(function (overload) {
            overload.implementation = function () {
                var result = overload.apply(this, arguments);
                report(result);
                return result;
            };
        });
        send({ kind: "hooked", value: "SecureStoreModule.getItemImpl" });
    }

    [hookDecryptor, hookModule].forEach(function (fn) {
        try {
            fn();
        } catch (e) {
            send({ kind: "note", value: String(e) });
        }
    });
});
"""


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", required=True, help="file to write the token into")
    ap.add_argument("--device", default="127.0.0.1:27042", help="frida-server endpoint")
    ap.add_argument("--package", default=PACKAGE, help="app package name")
    ap.add_argument("--timeout", type=int, default=90, help="seconds to wait")
    args = ap.parse_args()

    try:
        import frida
    except ImportError:
        print("error: the frida python package is not installed", file=sys.stderr)
        print('hint: pip install "frida==<version shown by frida-server --version>"', file=sys.stderr)
        return 2

    device = frida.get_device_manager().add_remote_device(args.device)

    captured: list[str] = []

    def on_message(message, _data):
        if message.get("type") == "send":
            payload = message.get("payload") or {}
            kind = payload.get("kind")
            if kind == "token":
                captured.append(payload["value"])
            elif kind == "hooked":
                print(f"hooked {payload.get('value')}", file=sys.stderr)
            elif kind == "note":
                print(f"note: {payload.get('value')}", file=sys.stderr)
        elif message.get("type") == "error":
            print(f"script error: {message.get('description')}", file=sys.stderr)

    # Spawn (not attach): the token is read during startup, so the hook has to be
    # in place before the process runs any of its own code.
    pid = device.spawn([args.package])
    session = device.attach(pid)
    script = session.create_script(AGENT)
    script.on("message", on_message)
    script.load()
    device.resume(pid)

    deadline = time.time() + args.timeout
    while time.time() < deadline and not captured:
        time.sleep(0.5)

    if not captured:
        print("error: no token captured", file=sys.stderr)
        print("hints:", file=sys.stderr)
        print("  - is frida-server running on the device, and its version equal to the client's?", file=sys.stderr)
        print("  - was the app already running? it must be started by this script", file=sys.stderr)
        return 1

    token = captured[0]
    with open(args.out, "w", encoding="utf-8") as fh:
        fh.write(token)
    print(f"wrote {len(token)} characters to {args.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
