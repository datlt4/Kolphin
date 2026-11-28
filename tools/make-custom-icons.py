#!/usr/bin/env python3
"""Build assets/custom-icons/<Group>/<Sub>/*.svg from Tabler Icons (MIT) and Simple Icons (CC0).

Usage: make-custom-icons.py <tabler package dir> <simple-icons package dir>
(npm tarballs @tabler/icons and simple-icons, extracted). Missing names are skipped.
"""
import colorsys
import json
import pathlib
import re
import shutil
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent / "assets" / "custom-icons"

# (group, sub, colour) -> Tabler outline icon names
TABLER = {
    ("Vehicles", "Cars & 4 wheels", "#4fc3f7"): """car car-suv car-4wd car-off-road car-turbine car-crane car-crash car-garage
        car-lifter car-door car-suspension car-fan car-fan-auto car-pin car-off camper caravan steering-wheel engine
        manual-gearbox battery-automotive gas-station charging-pile car-fan-1""",
    ("Vehicles", "Trucks & buses", "#ffb74d"): """truck truck-delivery truck-loading truck-return truck-off bus bus-stop bus-pin
        ambulance firetruck tractor forklift backhoe bulldozer excavator crane""",
    ("Vehicles", "Two wheels", "#81c784"): """bike bike-pin motorbike scooter scooter-electric moped helmet wheel
        skateboard skateboarding roller-skating""",
    ("Vehicles", "Rail & water", "#4dd0e1"): """train tram ship sailboat sailboat-2 speedboat kayak jetski submarine anchor buoy
        lifebuoy ferry""",
    ("Vehicles", "Road & traffic", "#ffd54f"): """road road-sign traffic-lights traffic-cone parking parking-circle barrier-block
        route route-2 directions gauge brand-speedtest arrow-roundabout-left arrow-roundabout-right speedboat map-route""",
    ("Drones & UAV", "Aircraft", "#90caf9"): """drone drone-off helicopter helicopter-landing plane plane-departure plane-arrival
        plane-tilt plane-inflight plane-off parachute air-balloon jetpack rocket ufo propeller air-traffic-control windsock""",
    ("Drones & UAV", "Navigation & control", "#a5d6a7"): """gps current-location map-pin map-pins compass radar radar-2 satellite
        antenna antenna-bars-5 broadcast world-latitude world-longitude navigation navigation-pin target crosshair
        device-gamepad-2 joystick battery-charging battery-4 location map-2 topology-star-3""",
    ("Cameras & media", "Cameras", "#f48fb1"): """camera camera-ai camera-selfie camera-rotate camera-search camera-bolt camera-check
        camera-pin camera-off device-cctv device-cctv-off device-computer-camera device-3d-camera device-3d-lens
        device-camera-phone aperture focus focus-2 focus-centered photo photo-scan photo-video panorama-horizontal
        360-view capture lens""",
    ("Cameras & media", "Video", "#ce93d8"): """video video-plus video-off movie movie-off film player-play player-record
        player-pause player-stop player-track-next device-tv device-tv-old device-projector brand-youtube-kids clapperboard
        badge-4k badge-hd live-photo live-view broadcast-off slideshow""",
    ("Cameras & media", "Audio", "#b39ddb"): """microphone microphone-2 headphones headset device-speaker music music-heart
        vinyl radio playlist volume volume-2 wave-sine waveform device-airpods antenna-bars-3 piano guitar-pick""",
    ("Security", "Cyber security", "#ef5350"): """shield shield-lock shield-check shield-x shield-half shield-bolt shield-search
        lock lock-open lock-access key password fingerprint fingerprint-scan face-id scan-eye virus virus-search bug
        firewall-check firewall-flame spy incognito user-shield user-scan server-2 server-bolt network cloud-lock
        alert-triangle biohazard skull bomb eye eye-off qrcode barcode id certificate file-lock folder-lock home-shield
        wifi router binary terminal-2 code brand-tor shield-checkered lock-password key-off""",
    ("Security", "Surveillance", "#ff8a65"): """device-cctv eye-search body-scan scan scan-position ear-scan user-search
        alarm alarm-light bell-ringing siren urgent police-car door-lock motion-sensor""",
    ("AI & robotics", "Robots & chips", "#80cbc4"): """robot robot-face cpu cpu-2 brain circuit-motor circuit-battery
        cloud-computing chart-dots-3 sparkles ai topology-ring device-desktop-analytics server cpu-off""",
}

# (group, sub) -> Simple Icons slugs; coloured with each brand's own hex.
SIMPLE = {
    ("Brands", "Cars"): """toyota honda hyundai kia mazda nissan mitsubishi subaru suzuki infiniti acura bmw audi volkswagen
        porsche opel skoda seat peugeot citroen renault dacia fiat ferrari lamborghini maserati astonmartin bentley
        rollsroyce mclaren bugatti mini volvo polestar tesla ford chevrolet cadillac jeep ram chrysler lucid tata
        mahindra smart koenigsegg generalmotors""",
    ("Brands", "Motorbikes"): "ducati ktm vespa piaggiogroup yamahamotorcorporation",
    ("Brands", "Aero & drones"): "dji airbus boeing spacex nasa garmin insta360",
    ("Brands", "Mobility & maps"): "uber grab lyft gojek waze googlemaps openstreetmap here mapbox",
    ("Brands", "Cameras & media"): """nikon sony fujifilm leica panasonic godox blackmagicdesign davinciresolve obsstudio
        vlcmediaplayer ffmpeg youtube netflix spotify twitch plex jellyfin kodi emby zoom""",
    ("Brands", "Security"): """kalilinux parrotsecurity wireshark metasploit burpsuite owasp hackthebox tryhackme cloudflare
        fortinet paloaltonetworks bitwarden 1password keepassxc protonvpn wireguard openvpn torproject letsencrypt snyk
        virustotal nordvpn mullvad yubico vault openssl gnuprivacyguard kaspersky norton avast mcafee bitdefender
        splunk okta auth0 cisco googleauthenticator""",
    ("Brands", "Smart home & cameras"): "ring wyze arlo ubiquiti googlehome",
    ("Brands", "AI & robotics"): "nvidia qualcomm ros",
}


def dirname(label: str) -> str:
    return label.replace(" & ", "_and_").replace(" ", "_")


def readable(hex_: str) -> str:
    """Brand colour lifted until it shows on the dark theme."""
    r, g, b = (int(hex_[i:i + 2], 16) / 255 for i in (0, 2, 4))
    h, l, s = colorsys.rgb_to_hls(r, g, b)
    l = max(l, 0.55 if s > 0.15 else 0.85)
    r, g, b = colorsys.hls_to_rgb(h, l, s)
    return "#%02x%02x%02x" % (round(r * 255), round(g * 255), round(b * 255))


def main(tabler: pathlib.Path, simple: pathlib.Path) -> None:
    material = {p.stem for p in (ROOT.parent / "material-icons" / "icons").glob("*.svg")}
    for d in ROOT.iterdir():
        if d.is_dir():
            shutil.rmtree(d)
    used, ours, skipped = set(material) | {p.stem for p in ROOT.glob("*.svg")}, set(), []

    def put(group: str, sub: str, name: str, svg: str) -> None:
        # Listed twice: keep the first. Same name as a Material icon: suffix it so file icons don't change.
        if name in ours:
            return
        ours.add(name)
        if name in used:
            name = f"{name}-{group.split()[0].lower()}"
        used.add(name)
        # Dirs without spaces: "Cars & 4 wheels" -> "Cars_and_4_wheels" (the app turns it back).
        out = ROOT / dirname(group) / dirname(sub) / f"{name}.svg"
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(svg)

    for (group, sub, colour), names in TABLER.items():
        for n in dict.fromkeys(names.split()):
            src = tabler / "icons" / "outline" / f"{n}.svg"
            if not src.exists():
                skipped.append(n)
                continue
            svg = re.sub(r"<!--.*?-->", "", src.read_text(), flags=re.S).strip()
            svg = svg.replace("currentColor", colour).replace(' class="icon icon-tabler icons-tabler-outline"', "")
            svg = re.sub(r'class="[^"]*"', "", svg)
            put(group, sub, n, svg)

    meta = {i["slug"]: i["hex"] for i in json.loads((simple / "data" / "simple-icons.json").read_text())}
    for (group, sub), slugs in SIMPLE.items():
        for slug in dict.fromkeys(slugs.split()):
            src = simple / "icons" / f"{slug}.svg"
            if not src.exists():
                skipped.append(slug)
                continue
            svg = src.read_text().replace("<path ", f'<path fill="{readable(meta.get(slug, "DDDDDD"))}" ', 1)
            svg = re.sub(r"<title>.*?</title>", "", svg)
            put(group, sub, f"logo-{slug}", svg)
    print("skipped:", " ".join(skipped))


if __name__ == "__main__":
    main(pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2]))
