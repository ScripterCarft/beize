//! The time zone, found by city, country or zone name in the tz database's
//! own tables: zone1970.tab (the zones and the countries they cover) and
//! iso3166.tab (country names). "Berlin", "Germany", "de" and
//! "Europe/Berlin" all lead to Europe/Berlin; "Brazil" asks which zone.

pub const ZONEINFO: &str = "/usr/share/zoneinfo";

#[derive(Debug, PartialEq)]
pub enum Found {
    Zone(String),
    /// The answer fits several zones (a country spanning time zones, or a
    /// city name used twice).
    Several(Vec<String>),
    None,
}

/// Looks `answer` up; case and spaces versus underscores do not matter.
pub fn find(answer: &str, zone_tab: &str, country_tab: &str) -> Found {
    let wanted = normalize(answer);
    if wanted.is_empty() {
        return Found::None;
    }
    if wanted == "utc" || wanted == "etc/utc" {
        return Found::Zone("Etc/UTC".to_owned());
    }
    let zones: Vec<(Vec<&str>, &str)> = rows(zone_tab)
        .filter_map(|fields| Some((fields.first()?.split(',').collect(), *fields.get(2)?)))
        .collect();
    if let Some((_, zone)) = zones.iter().find(|(_, zone)| normalize(zone) == wanted) {
        return Found::Zone((*zone).to_owned());
    }
    let mut matches: Vec<String> = zones
        .iter()
        .filter(|(_, zone)| zone.rsplit('/').next().map(normalize) == Some(wanted.clone()))
        .map(|(_, zone)| (*zone).to_owned())
        .collect();
    if matches.is_empty() {
        let country = rows(country_tab).find_map(|fields| {
            let (code, name) = (fields.first()?, fields.get(1)?);
            (normalize(code) == wanted || normalize(name) == wanted).then_some(*code)
        });
        if let Some(code) = country {
            matches = zones
                .iter()
                .filter(|(codes, _)| codes.contains(&code))
                .map(|(_, zone)| (*zone).to_owned())
                .collect();
        }
    }
    matches.sort();
    matches.dedup();
    match matches.len() {
        0 => Found::None,
        1 => Found::Zone(matches.remove(0)),
        _ => Found::Several(matches),
    }
}

/// The tab-separated rows of a tz table, without comments.
fn rows(table: &str) -> impl Iterator<Item = Vec<&str>> {
    table
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| line.split('\t').collect())
}

fn normalize(text: &str) -> String {
    text.trim().to_lowercase().replace(' ', "_")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ZONES: &str = "# comment\n\
        DE,DK,NO,SE,SJ\t+5230+01322\tEurope/Berlin\tmost of Germany\n\
        BR\t-2332-04637\tAmerica/Sao_Paulo\tBrazil (southeast)\n\
        BR\t-0343-03830\tAmerica/Fortaleza\tBrazil (northeast)\n\
        US\t+404251-0740023\tAmerica/New_York\tEastern (most areas)\n\
        US\t+4005-08245\tAmerica/Indiana/Indianapolis\tEastern - IN\n";
    const COUNTRIES: &str = "# comment\nBR\tBrazil\nDE\tGermany\nUS\tUnited States\n";

    fn find(answer: &str) -> Found {
        super::find(answer, ZONES, COUNTRIES)
    }

    fn zone(name: &str) -> Found {
        Found::Zone(name.to_owned())
    }

    #[test]
    fn finds_zones_by_name_city_and_country() {
        assert_eq!(find("Europe/Berlin"), zone("Europe/Berlin"));
        assert_eq!(find("berlin"), zone("Europe/Berlin"));
        assert_eq!(find(" Germany "), zone("Europe/Berlin"));
        assert_eq!(find("de"), zone("Europe/Berlin"));
        assert_eq!(find("Sao Paulo"), zone("America/Sao_Paulo"));
        assert_eq!(find("indianapolis"), zone("America/Indiana/Indianapolis"));
        assert_eq!(find("UTC"), zone("Etc/UTC"));
    }

    #[test]
    fn countries_with_several_zones_ask_back() {
        assert_eq!(
            find("Brazil"),
            Found::Several(vec![
                "America/Fortaleza".to_owned(),
                "America/Sao_Paulo".to_owned()
            ])
        );
        assert_eq!(
            find("united states"),
            Found::Several(vec![
                "America/Indiana/Indianapolis".to_owned(),
                "America/New_York".to_owned()
            ])
        );
    }

    #[test]
    fn unknown_answers_find_nothing() {
        assert_eq!(find(""), Found::None);
        assert_eq!(find("Atlantis"), Found::None);
        // Only whole names count, no partial matches.
        assert_eq!(find("Berl"), Found::None);
    }
}
