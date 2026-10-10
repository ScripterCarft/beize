# 0011 – Netzwerk: systemd-networkd und systemd-resolved

Status: angenommen · 2026-10-09 (Zeitsynchronisation: ADR 0012)

## Kontext

Beize braucht Netzwerk – zuerst über LAN (Intel I225/I226, Treiber `igc`), später
WLAN. systemd-networkd und -resolved werden mit systemd ohnehin gebaut und sind per
Preset aktiviert; ohne Konfiguration ließ networkd den Anschluss aber unkonfiguriert. Zu entscheiden war außerdem, welche Server Beize von
sich aus kontaktiert: systemd bringt fest eingebaute Ausweich-Server mit
(DNS: Cloudflare, Google, Quad9; Zeit: Google).

## Entscheidung

- **networkd + resolved**, kein NetworkManager/ConnMan: klein, Teil des schon
  vorhandenen systemd, per D-Bus/Varlink steuerbar – die spätere Einstellungen-App
  spricht direkt mit ihnen. Ob WLAN über networkd + iwd läuft, wird mit WLAN
  entschieden.
- `/usr/lib/systemd/network/80-beize-ethernet.network`: jeder physische
  Ethernet-Anschluss (`Type=ether`, `Kind=!*`) per DHCPv4 und IPv6-Router-Advertisements.
  DNS- und NTP-Server kommen vom Netzwerk.
- **Keine eingebauten Ausweich-Server** (`-Ddns-servers= -Dntp-servers=`): Beize
  kontaktiert niemanden, den das Netzwerk oder der Besitzer nicht genannt hat
  (VISION: keine Telemetrie). Liefert ein Netz kein DNS, gehen Namen sichtbar nicht
  auf, statt still über Dritte zu laufen. Eigene DNS-Server einstellen (wie unter
  Windows „Router“ oder „manuell“) bekommt die Einstellungen-App; technisch ist
  das `DNS=` pro Verbindung bzw. in resolved.conf.
- Namensauflösung über **nss-resolve** (`/etc/nsswitch.conf` aus glibc nennt es
  schon), `/etc/resolv.conf` zeigt auf resolveds Stub (tmpfiles von systemd) für
  Programme, die selbst DNS sprechen. DNSSEC bleibt aus (viele Router brechen es).
- **LLMNR aus** (`-Ddefault-llmnr=no`): fälschbar (Angriffe à la Responder),
  Microsoft stellt es zugunsten von mDNS ein; Namen im lokalen Netz löst später mDNS.
- **IPv6-Privatsphäre wie bei Windows/macOS:** stabile Adresse pro Netz ohne
  MAC-Adresse (RFC 7217, `Token=prefixstable`) und kurzlebige Zufallsadressen für
  ausgehende Verbindungen (RFC 8981, `IPv6PrivacyExtensions=yes` in
  `networkd.conf.d/beize.conf`). networkd würde sonst EUI-64 (MAC-basiert) nutzen.
- Nichts im Start wartet auf das Netzwerk (`network-online.target` zieht niemand
  herein) – der Start bleibt unabhängig von DHCP.

## Konsequenzen

- `bz smoke` startet jede VM mit einer virtio-Netzwerkkarte hinter QEMUs NAT und
  verlangt bei jedem gesunden Start: Zustand `routable`, als DNS genau den Server
  aus DHCP (10.0.2.3), und `_gateway` über NSS aufgelöst (nss-resolve). Kein
  Internet nötig, der Test bleibt offline stabil.
- Auf dem Rechner des Besitzers läuft LAN über `igc` (Intel 2.5GbE) mit dieser
  Konfiguration.
