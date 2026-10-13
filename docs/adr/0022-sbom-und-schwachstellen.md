# 0022 – SBOM und Schwachstellen

Status: angenommen · 2026-10-13 (mit dem Besitzer abgestimmt)

## Kontext

Beize baut jedes Paket selbst aus gepinnten, signierten Quellen (ADR 0005). Was in einem
Image steckt, ließ sich bisher aber nur aus den Rezepten zusammensuchen, und ob eine
gepinnte Version eine bekannte Lücke hat, prüfte niemand: `bz outdated` meldet neue
Versionen, keine Schwachstellen.

Große Distributionen veröffentlichen zu jedem Stand eine Stückliste (SBOM; RHEL, SUSE und
Ubuntu als SPDX) und gleichen ihre Pakete mit den CVE-Datenbanken ab. Yocto macht beides aus
den Rezepten: `create-spdx` und `cve-check` mit `CVE_PRODUCT` je Rezept und `CVE_STATUS`
für bewertete Einträge. Der Cyber Resilience Act der EU verlangt beides für Produkte mit
digitalen Elementen.

## Entscheidung

- **Rezepte tragen Lizenz und Produktnamen** im Abschnitt `[sbom]` (vor `[files]`; beide
  sind Metadaten und zählen nicht zum Bau-Fingerprint):
  - `license`: SPDX-Ausdruck, wie Fedora ihn für das Paket festgestellt hat (für Teile
    eines Pakets der Teil). Eigene Bezeichner (`LicenseRef-…`) erklärt die SBOM.
  - `cpe`: Liste der CPE-2.3-Präfixe aus dem NVD-Wörterbuch
    (`cpe:2.3:<a|o>:<hersteller>:<produkt>`), mehrere wie Yoctos `CVE_PRODUCT`. Sie ist
    leer mit Begründung für Daten und Firmware, die die NVD nicht als Produkt führt.

  Beides ist Pflicht für jede Quelle; ein Test verlangt es. Beizes eigener Code hat
  `NOASSERTION`, bis der Besitzer eine Lizenz gewählt hat.
- **SBOM je Image.** `bz` schreibt beim Image-Schritt `build/out/sbom.spdx.json` (SPDX 2.3,
  datiert mit `SOURCE_DATE_EPOCH`). Darin stehen:
  - jedes Paket des Images, Kernel, Firmware und Microcode mit Version, Quelle, Pin
    (SHA-256 bzw. Commit), Lizenz und CPE;
  - Abhängigkeiten aus `deps`; Toolchain und reine Bau-Pakete als Bauwerkzeuge;
  - die Rust-Crates der eigenen Dienste (aus `Cargo.lock`);
  - die Pakete des Buildroots (ADR 0021) als Bauwerkzeuge;
  - bewertete CVEs als Anmerkung.

  `bz release` legt die SBOM neben das Update.
- **Schwachstellen.** `bz cve` fragt die NVD (API 2.0) je CPE und Version ab und meldet
  offene CVEs. Bewertete Einträge stehen im Rezept unter `[sbom.cve-ignore]` mit Grund
  (wie Yoctos `CVE_STATUS`), etwa „betrifft nur Windows“ oder „im Rezept behoben“. Der
  wöchentliche CI-Lauf führt es aus und scheitert bei neuen Einträgen ab CVSS 7.0.
- **Rust-Abhängigkeiten.** cargo-deny prüft in jedem CI-Lauf RustSec-Advisories, Lizenzen
  (Erlaubnisliste) und Herkunft (nur crates.io) (`deny.toml`).

## Konsequenzen

- Zu jedem Stand gibt es eine maschinenlesbare Stückliste, und bekannte Lücken fallen
  spätestens nach einer Woche auf, nicht erst bei `bz outdated`.
- Die NVD-Abfrage ist ein Netzdienst ohne Signatur; sie meldet nur und ändert nichts am Bau.
- CPE-Namen sind ungenau (Produkte ohne CPE, Sammelnamen). Was die NVD nicht kennt, prüft
  die wöchentliche Upstream-Kontrolle (`bz outdated`) weiter mit.
- Offen: die Crates in ntpd-rs (eigenes `Cargo.lock` in seiner Quelle) stehen noch nicht
  in der SBOM und nicht unter cargo-deny.
