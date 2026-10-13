#!/usr/bin/env python3
"""Deploy to an Xbox in Dev Mode through the Device Portal REST API (loose-folder registration,
no packaging or signing needed), launch, fetch the app log and take screenshots.

    tools/xbox.py deploy            build output + pkg/ -> upload, register, launch (resets app type to App)
    tools/xbox.py update            replace the exe only and relaunch (keeps Game mode)
    tools/xbox.py launch | stop
    tools/xbox.py log               print LocalState/log.txt
    tools/xbox.py push FILE [NAME]  stream a file into LocalState (multi-GB is fine)
    tools/xbox.py rm NAME | ls      delete a LocalState file / list them
    tools/xbox.py cat NAME          print a LocalState file
    tools/xbox.py screenshot out.png

The console address comes from $XBOX (default 192.168.1.6:11443).
"""
import base64
import http.cookiejar
import json
import os
import ssl
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
HOST = os.environ.get("XBOX", "192.168.1.6:11443")
BASE = f"https://{HOST}"
PKG_NAME = "BlinkTerm.Terminal"
FOLDER = "BlinkTerm"
EXE = os.path.join(ROOT, "target", "x86_64-pc-windows-msvc", "release", "blink-term.exe")

ctx = ssl.create_default_context()
ctx.check_hostname = False
ctx.verify_mode = ssl.CERT_NONE  # the portal uses a self-signed certificate
jar = http.cookiejar.CookieJar()
opener = urllib.request.build_opener(urllib.request.HTTPSHandler(context=ctx), urllib.request.HTTPCookieProcessor(jar))


def b64(s):
    return base64.b64encode(s.encode()).decode()


def request(method, path, params=None, body=None, headers=None):
    url = BASE + path + ("?" + urllib.parse.urlencode(params) if params else "")
    h = dict(headers or {})
    if method != "GET":
        if not any(c.name == "CSRF-Token" for c in jar):
            request("GET", "/api/os/info")
        for c in jar:
            if c.name == "CSRF-Token":
                h["X-CSRF-Token"] = c.value
    req = urllib.request.Request(url, data=body, method=method, headers=h)
    try:
        with opener.open(req, timeout=120) as r:
            return r.status, r.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()


def get_json(path, params=None):
    status, data = request("GET", path, params)
    if status != 200:
        sys.exit(f"GET {path}: HTTP {status} {data[:300]!r}")
    return json.loads(data)


def find_package():
    for p in get_json("/api/app/packagemanager/packages")["InstalledPackages"]:
        if p["PackageFullName"].startswith(PKG_NAME + "_"):
            return p
    return None


def upload(dest, files):
    boundary = uuid.uuid4().hex
    body = bytearray()
    for path in files:
        name = os.path.basename(path)
        body += f"--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n".encode()
        with open(path, "rb") as f:
            body += f.read()
        body += b"\r\n"
    body += f"--{boundary}--\r\n".encode()
    status, data = request("POST", "/api/app/packagemanager/upload", {"destinationFolder": b64(dest)}, bytes(body), {"Content-Type": f"multipart/form-data; boundary={boundary}"})
    if status not in (200, 202):
        sys.exit(f"upload {dest}: HTTP {status} {data[:500]!r}")
    print(f"uploaded {len(files)} file(s) to {dest}")


def stop():
    p = find_package()
    if p:
        status, _ = request("DELETE", "/api/taskmanager/app", {"package": b64(p["PackageFullName"])})
        print(f"stop: HTTP {status}")


def launch():
    p = find_package()
    if not p:
        sys.exit("not installed")
    status, data = request("POST", "/api/taskmanager/app", {"appid": b64(p["PackageRelativeId"])})
    print(f"launch {p['PackageRelativeId']}: HTTP {status} {data[:200].decode(errors='replace')}")


def deploy():
    if not os.path.exists(EXE):
        sys.exit(f"missing {EXE}; run cargo build --release first")
    pkg = os.path.join(ROOT, "pkg")
    stop()
    upload(FOLDER, [os.path.join(pkg, "AppxManifest.xml"), EXE])
    assets = os.path.join(pkg, "Assets")
    upload(FOLDER + "/Assets", sorted(os.path.join(assets, f) for f in os.listdir(assets)))
    status, data = request("POST", "/api/app/packagemanager/register", {"folder": b64(FOLDER)})
    if status not in (200, 202, 204):
        sys.exit(f"register: HTTP {status} {data[:800].decode(errors='replace')}")
    print("registering...")
    for _ in range(120):
        time.sleep(1)
        status, data = request("GET", "/api/app/packagemanager/state")
        if status == 204:
            continue
        if status == 200:
            state = json.loads(data) if data else {}
            if state.get("Success") is False or (state.get("Code") not in (None, 0)):
                sys.exit(f"registration failed: {state}")
            print(f"registered: {state or 'ok'}")
            break
        if status == 404:
            print("registered (no install state)")
            break
        sys.exit(f"state: HTTP {status} {data[:500]!r}")
    launch()


def update():
    """Replace only the executable in the registered loose folder. Re-registering resets the
    app type to App, so this keeps Game mode as long as the manifest and assets are unchanged."""
    if not find_package():
        sys.exit("not installed; run deploy first")
    stop()
    time.sleep(2)
    upload(FOLDER, [EXE])
    launch()


def local_state_params(**extra):
    p = find_package()
    if not p:
        sys.exit("not installed")
    return {"knownfolderid": "LocalAppData", "packagefullname": p["PackageFullName"], "path": "\\LocalState", **extra}


def push(local, remote=None):
    """Stream a (possibly multi-GB) file into the app's LocalState folder."""
    import http.client
    remote = remote or os.path.basename(local)
    size = os.path.getsize(local)
    boundary = uuid.uuid4().hex
    head = f"--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{remote}\"\r\nContent-Type: application/octet-stream\r\n\r\n".encode()
    tail = f"\r\n--{boundary}--\r\n".encode()
    request("GET", "/api/os/info")
    token = next((c.value for c in jar if c.name == "CSRF-Token"), "")
    cookies = "; ".join(f"{c.name}={c.value}" for c in jar)
    host, port = HOST.split(":")
    conn = http.client.HTTPSConnection(host, int(port), context=ctx, timeout=600)
    conn.putrequest("POST", "/api/filesystem/apps/file?" + urllib.parse.urlencode(local_state_params()))
    conn.putheader("Content-Type", f"multipart/form-data; boundary={boundary}")
    conn.putheader("Content-Length", str(len(head) + size + len(tail)))
    conn.putheader("X-CSRF-Token", token)
    conn.putheader("Cookie", cookies)
    conn.endheaders()
    conn.send(head)
    sent, t0, last = 0, time.time(), 0
    with open(local, "rb") as f:
        while chunk := f.read(8 << 20):
            conn.send(chunk)
            sent += len(chunk)
            if time.time() - last > 5:
                last = time.time()
                print(f"  {sent >> 20} / {size >> 20} MB, {sent / (last - t0) / 1e6:.0f} MB/s", flush=True)
    conn.send(tail)
    r = conn.getresponse()
    body = r.read()
    print(f"push {remote}: HTTP {r.status} {body[:300]!r} in {time.time() - t0:.0f}s")
    if r.status not in (200, 202):
        sys.exit(1)


def rm(remote):
    status, data = request("DELETE", "/api/filesystem/apps/file", local_state_params(filename=remote))
    print(f"rm {remote}: HTTP {status} {data[:200]!r}")


def ls():
    for f in get_json("/api/filesystem/apps/files", local_state_params()).get("Items", []):
        print(f"{f.get('FileSize', 0) >> 20:8d} MB  {f['Name']}")


def cat(name):
    status, data = request("GET", "/api/filesystem/apps/file", local_state_params(filename=name))
    if status != 200:
        sys.exit(f"{name}: HTTP {status} {data[:300]!r}")
    sys.stdout.write(data.decode(errors="replace"))


def app_log():
    cat("log.txt")


def screenshot(out):
    status, data = request("GET", "/ext/screenshot", {"download": "true", "hdr": "false"})
    if status != 200:
        sys.exit(f"screenshot: HTTP {status} {data[:300]!r}")
    with open(out, "wb") as f:
        f.write(data)
    print(f"wrote {out} ({len(data)} bytes)")


if __name__ == "__main__":
    cmd = sys.argv[1] if len(sys.argv) > 1 else "deploy"
    if cmd == "deploy":
        deploy()
    elif cmd == "update":
        update()
    elif cmd == "launch":
        launch()
    elif cmd == "stop":
        stop()
    elif cmd == "push":
        push(sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else None)
    elif cmd == "rm":
        rm(sys.argv[2])
    elif cmd == "ls":
        ls()
    elif cmd == "cat":
        cat(sys.argv[2])
    elif cmd == "log":
        app_log()
    elif cmd == "screenshot":
        screenshot(sys.argv[2] if len(sys.argv) > 2 else "screenshot.png")
    else:
        sys.exit(__doc__)
