"""Reproduz a sequência de requisições típica de clientes Jellyfin (Swiftfin/Findroid/Moonfin/web)."""
import json, sys, urllib.request, urllib.error

base = sys.argv[1]
AUTH = 'MediaBrowser Client="Swiftfin", Device="iPhone", DeviceId="TEST-DEVICE-1", Version="1.0.0"'
token = None

def call(method, path, body=None, expect=(200, 204), label=None, raw=False, headers=None):
    h = {"Authorization": AUTH + (f', Token="{token}"' if token else ""),
         "X-Emby-Authorization": AUTH, "Accept": "application/json"}
    if headers: h.update(headers)
    data = None
    if body is not None:
        data = json.dumps(body).encode(); h["Content-Type"] = "application/json"
    req = urllib.request.Request(base + path, data=data, method=method, headers=h)
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            code, payload, ctype = r.status, r.read(), r.headers.get("Content-Type", "")
    except urllib.error.HTTPError as e:
        code, payload, ctype = e.code, e.read(), e.headers.get("Content-Type", "")
    ok = code in expect
    mark = "OK  " if ok else "FALHA"
    parsed = None
    if payload and "json" in ctype.lower() and not raw:
        try: parsed = json.loads(payload)
        except Exception: pass
    print(f"[{mark}] {method:6} {path[:78]:78} -> {code} {ctype.split(';')[0]}")
    return code, parsed, payload

print("--- 1. Descoberta / conexão")
call("GET", "/System/Info/Public")
call("GET", "/System/Ping")
call("GET", "/QuickConnect/Enabled")
call("GET", "/Branding/Configuration")
call("GET", "/Branding/Css")
call("GET", "/Users/Public")

print("--- 2. Login")
_, auth, _ = call("POST", "/Users/AuthenticateByName", {"Username": "Admin", "Pw": ""})
token = auth["AccessToken"]; uid = auth["User"]["Id"]
call("GET", "/Users/Me")
call("GET", f"/Users/{uid}")
call("POST", "/Sessions/Capabilities/Full", {"PlayableMediaTypes": ["Video"], "SupportsMediaControl": False})
call("GET", "/System/Info")

print("--- 3. Tela inicial")
_, views, _ = call("GET", "/UserViews?userId=" + uid)
call("GET", f"/Users/{uid}/Views")
call("GET", "/Library/MediaFolders")
call("GET", "/Library/VirtualFolders")
call("GET", f"/DisplayPreferences/usersettings?userId={uid}&client=emby")
call("GET", f"/UserItems/Resume?userId={uid}&Limit=12&MediaTypes=Video&Fields=PrimaryImageAspectRatio")
call("GET", f"/Users/{uid}/Items/Resume?Limit=12&MediaTypes=Video")
call("GET", f"/Shows/NextUp?userId={uid}&Limit=24")
for v in (views or {}).get("Items", []):
    call("GET", f"/Items/Latest?userId={uid}&ParentId={v['Id']}&Limit=16")
    call("GET", f"/Users/{uid}/Items/Latest?ParentId={v['Id']}&Limit=16")

print("--- 4. Biblioteca")
movies = [v for v in views["Items"] if v.get("CollectionType") == "movies"][0]
shows = [v for v in views["Items"] if v.get("CollectionType") == "tvshows"][0]
_, ml, _ = call("GET", f"/Items?userId={uid}&ParentId={movies['Id']}&SortBy=SortName&SortOrder=Ascending&IncludeItemTypes=Movie&Recursive=true&Fields=PrimaryImageAspectRatio,Overview&StartIndex=0&Limit=60")
_, sl, _ = call("GET", f"/Items?userId={uid}&ParentId={shows['Id']}&SortBy=SortName&IncludeItemTypes=Series&Recursive=true&Limit=60")
call("GET", f"/Genres?userId={uid}&ParentId={movies['Id']}")
call("GET", f"/Studios?userId={uid}&ParentId={movies['Id']}")
call("GET", f"/Persons?userId={uid}")
call("GET", f"/Items/Filters?userId={uid}&ParentId={movies['Id']}")
call("GET", f"/Items/Filters2?userId={uid}&ParentId={movies['Id']}")

print("--- 5. Detalhe de filme")
mid = ml["Items"][0]["Id"]
call("GET", f"/Items/{mid}?userId={uid}")
call("GET", f"/Users/{uid}/Items/{mid}")
call("GET", f"/Items/{mid}/Similar?userId={uid}&Limit=12")
call("GET", f"/Items/{mid}/Images/Primary?maxWidth=400&tag=x", expect=(200, 404))
call("GET", f"/Items/{mid}/Images/Backdrop/0", expect=(200, 404))
call("GET", f"/Items/{mid}/Intros", expect=(200,))
call("GET", f"/Items/{mid}/ThemeMedia", expect=(200,))
call("GET", f"/MediaSegments/{mid}", expect=(200,))

print("--- 6. Série")
sid = sl["Items"][0]["Id"]
call("GET", f"/Items/{sid}?userId={uid}")
_, ss, _ = call("GET", f"/Shows/{sid}/Seasons?userId={uid}")
call("GET", f"/Shows/{sid}/Episodes?userId={uid}&seasonId={ss['Items'][0]['Id']}&Fields=Overview")
call("GET", f"/Items/{sid}/Similar?userId={uid}&Limit=12")

print("--- 7. Reprodução")
_, pi, _ = call("POST", f"/Items/{mid}/PlaybackInfo?userId={uid}", {"DeviceProfile": {"Name": "Swiftfin", "DirectPlayProfiles": [{"Type": "Video", "Container": "mkv,mp4"}]}})
call("GET", f"/Items/{mid}/PlaybackInfo?userId={uid}")
src = pi["MediaSources"][0]
print("      DirectStreamUrl:", src.get("DirectStreamUrl"), "| SupportsDirectPlay:", src["SupportsDirectPlay"], "| PlaySessionId:", bool(pi.get("PlaySessionId")))
call("POST", "/Sessions/Playing", {"ItemId": mid, "PlaySessionId": pi["PlaySessionId"], "MediaSourceId": src["Id"], "CanSeek": True})
call("GET", f"/Videos/{mid}/stream?static=true&mediaSourceId={src['Id']}&PlaySessionId={pi['PlaySessionId']}", raw=True)
call("GET", f"/Videos/{mid}/stream.mkv?static=true", raw=True, headers={"Range": "bytes=0-99"}, expect=(206,))
call("GET", f"/Videos/{mid}/stream", raw=True, headers={"Range": "bytes=9999999-"}, expect=(416,))
call("GET", f"/Items/{mid}/Download", raw=True)
call("POST", "/Sessions/Playing/Progress", {"ItemId": mid, "PositionTicks": 12345, "IsPaused": False, "PlaySessionId": pi["PlaySessionId"]})
call("POST", "/Sessions/Playing/Ping?playSessionId=" + pi["PlaySessionId"])
call("POST", "/Sessions/Playing/Stopped", {"ItemId": mid, "PositionTicks": 12345, "PlaySessionId": pi["PlaySessionId"]})
call("GET", "/Playback/BitrateTest?size=100000", raw=True)

print("--- 8. Marcar assistido / favorito")
call("POST", f"/UserPlayedItems/{mid}?userId={uid}")
_, it, _ = call("GET", f"/Items/{mid}?userId={uid}")
print("      Played após marcar:", it["UserData"]["Played"])
call("DELETE", f"/UserPlayedItems/{mid}?userId={uid}")
call("POST", f"/Users/{uid}/PlayedItems/{mid}")
call("DELETE", f"/Users/{uid}/PlayedItems/{mid}")
call("POST", f"/UserFavoriteItems/{mid}?userId={uid}")
call("DELETE", f"/UserFavoriteItems/{mid}?userId={uid}")
call("GET", "/Sessions")
call("POST", "/Sessions/Logout", expect=(200, 204))
