//! Interpreta nomes de arquivo/pastas: título limpo, ano e (para séries) temporada/episódio.

use regex::Regex;
use std::{path::Path, sync::OnceLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Movie,
    Episode {
        series: String,
        season: i32,
        episode: i32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parsed {
    /// Título limpo (filme) ou título do episódio quando conhecido; senão o nome da série/filme.
    pub title: String,
    pub year: Option<i32>,
    pub kind: Kind,
}

/// Termos de release que marcam o fim do título.
const RELEASE_TAGS: &[&str] = &[
    "480p", "576p", "720p", "1080p", "1080i", "2160p", "4k", "uhd", "bluray", "bdrip", "brrip",
    "blu-ray", "webrip", "web-dl", "webdl", "web", "hdtv", "dvdrip", "dvd", "remux", "x264",
    "x265", "h264", "h265", "hevc", "av1", "hdr", "hdr10", "dv", "10bit", "aac", "ac3", "dts",
    "dual", "multi", "proper", "repack", "extended", "unrated", "remastered", "imax", "nf",
    "amzn", "dsnp",
];

fn episode_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // S01E02, s1e2, 1x02 (com separadores opcionais)
    RE.get_or_init(|| Regex::new(r"(?i)(?:\bs(\d{1,2})[ ._-]?e(\d{1,3})|\b(\d{1,2})x(\d{2,3}))\b").unwrap())
}

fn long_episode_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // "Temporada 1 Episódio 2", "Season 1 Episode 2", "Temp 1 Ep 2", "T1 E2"
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:temporada|temp|season|t)\s*(\d{1,2})\W{0,3}(?:epis[oó]dio|episode|ep|e)\s*(\d{1,3})\b",
        )
        .unwrap()
    })
}

/// Posição (início, fim) e (temporada, episódio) da marcação de episódio no texto normalizado.
fn find_episode(text: &str) -> Option<(usize, usize, i32, i32)> {
    if let Some(caps) = episode_re().captures(text) {
        let m = caps.get(0)?;
        let (season, episode) = match (caps.get(1), caps.get(2), caps.get(3), caps.get(4)) {
            (Some(s), Some(e), _, _) | (_, _, Some(s), Some(e)) => (
                s.as_str().parse().unwrap_or(1),
                e.as_str().parse().unwrap_or(0),
            ),
            _ => (1, 0),
        };
        return Some((m.start(), m.end(), season, episode));
    }
    let caps = long_episode_re().captures(text)?;
    let m = caps.get(0)?;
    Some((
        m.start(),
        m.end(),
        caps[1].parse().unwrap_or(1),
        caps[2].parse().unwrap_or(0),
    ))
}

fn year_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?:^|[\s(\[])((?:19|20)\d{2})(?:$|[\s)\]])").unwrap())
}

fn season_dir_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)^(season|temporada|s)\s*\d+$|^specials?$").unwrap())
}

/// Troca separadores de arquivo por espaços e normaliza espaços.
fn normalize(raw: &str) -> String {
    let mut s = raw.to_string();
    // "Nome.Do.Filme.2020" → espaços. Só trocamos "." se não houver espaços (nome estilo release).
    if !s.contains(' ') {
        s = s.replace(['.', '_'], " ");
    } else {
        s = s.replace('_', " ");
    }
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Corta o texto no primeiro termo de release e remove colchetes/parênteses soltos.
fn strip_release_tags(text: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for word in text.split_whitespace() {
        let w = word
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '-')
            .to_lowercase();
        if RELEASE_TAGS.contains(&w.as_str()) {
            break;
        }
        out.push(word);
    }
    out.join(" ")
        .trim_matches(|c: char| c.is_whitespace() || "-([{".contains(c))
        .to_string()
}

fn clean_title(raw: &str) -> String {
    // Remove grupos entre colchetes de fansub/release: "[Group] Nome" → "Nome"
    static BRACKETS: OnceLock<Regex> = OnceLock::new();
    let re = BRACKETS.get_or_init(|| Regex::new(r"\[[^\]]*\]").unwrap());
    let without = re.replace_all(raw, " ");
    strip_release_tags(&normalize(&without))
}

/// `path` completo permite usar as pastas (`Serie/Season 1/ep.mkv`) quando o nome do arquivo não basta.
pub fn parse_path(path: &Path) -> Parsed {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let mut dirs = path
        .parent()
        .into_iter()
        .flat_map(|p| p.components().rev())
        .filter_map(|c| c.as_os_str().to_str());
    let parent = dirs.next();
    let grandparent = dirs.next();
    parse_name(stem, parent, grandparent)
}

pub fn parse_name(stem: &str, parent: Option<&str>, grandparent: Option<&str>) -> Parsed {
    let normalized = normalize(stem);

    if let Some((start, end, season, episode)) = find_episode(&normalized) {
        let before = clean_title(&normalized[..start]);
        let after = clean_title(&normalized[end..]);

        // Série: texto antes do SxxExx; senão a pasta (ignorando "Season 1").
        let series = if !before.is_empty() {
            before
        } else {
            let folder = match parent {
                Some(p) if !season_dir_re().is_match(p.trim()) => Some(p),
                _ => grandparent,
            };
            folder.map(clean_title).unwrap_or_default()
        };
        let series = if series.is_empty() { "Série Desconhecida".to_string() } else { series };

        let year = year_re()
            .captures(&series)
            .and_then(|c| c[1].parse().ok())
            // Nome do arquivo sem ano: usa o da pasta da série ("Ghosts (2019)/Season 1/...").
            .or_else(|| {
                let folder = match parent {
                    Some(p) if !season_dir_re().is_match(p.trim()) => Some(p),
                    _ => grandparent,
                }?;
                year_re().captures(&normalize(folder)).and_then(|c| c[1].parse().ok())
            });
        let series = strip_year(&series);
        let title = if after.is_empty() {
            format!("Episódio {episode}")
        } else {
            after
        };

        return Parsed {
            title,
            year,
            kind: Kind::Episode { series, season, episode },
        };
    }

    // Filme: último ano plausível delimita o título ("Blade Runner 2049 (2017)" → 2017).
    let cleaned_input = normalized.clone();
    let mut year = None;
    let mut cut = cleaned_input.len();
    for caps in year_re().captures_iter(&cleaned_input) {
        let g = caps.get(1).unwrap();
        // Ano no início do nome é parte do título ("2012", "1917"), não ano de lançamento.
        if g.start() > 0 {
            year = g.as_str().parse().ok();
            cut = g.start();
        }
    }
    let title_part = &cleaned_input[..cut];
    let mut title = clean_title(title_part);
    if title.is_empty() {
        title = clean_title(&cleaned_input);
    }
    if title.is_empty() {
        title = stem.to_string();
    }

    Parsed { title, year, kind: Kind::Movie }
}

fn strip_year(s: &str) -> String {
    let re = year_re();
    match re.captures(s).and_then(|c| c.get(1)) {
        Some(g) => {
            let mut out = s[..g.start()].to_string();
            out.push_str(&s[g.end()..]);
            out.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .trim_matches(|c: char| "([ )]".contains(c))
                .to_string()
        }
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn movie(name: &str) -> (String, Option<i32>) {
        let p = parse_name(name, None, None);
        assert_eq!(p.kind, Kind::Movie, "{name}");
        (p.title, p.year)
    }

    #[test]
    fn movies() {
        assert_eq!(movie("Blade Runner 2049 (2017)"), ("Blade Runner 2049".into(), Some(2017)));
        assert_eq!(
            movie("The.Matrix.1999.1080p.BluRay.x264-GRP"),
            ("The Matrix".into(), Some(1999))
        );
        assert_eq!(movie("Filme Sem Ano"), ("Filme Sem Ano".into(), None));
        assert_eq!(movie("2012"), ("2012".into(), None));
        assert_eq!(movie("Interstellar 2014 4K HDR"), ("Interstellar".into(), Some(2014)));
        assert_eq!(movie("WhatsApp Video 2026-11-05 at 15.17.09").1, None);
    }

    #[test]
    fn episodes() {
        let p = parse_name("Breaking.Bad.S02E05.720p.HDTV", None, None);
        assert_eq!(
            p.kind,
            Kind::Episode { series: "Breaking Bad".into(), season: 2, episode: 5 }
        );
        let p = parse_name("Show Name - S01E02 - O Piloto", None, None);
        assert_eq!(p.title, "O Piloto");
        assert_eq!(
            p.kind,
            Kind::Episode { series: "Show Name".into(), season: 1, episode: 2 }
        );
        let p = parse_name("Lost 1x04", None, None);
        assert_eq!(p.kind, Kind::Episode { series: "Lost".into(), season: 1, episode: 4 });
    }

    #[test]
    fn long_form_episodes_pt_and_en() {
        let p = parse_name("BLEACH Temporada 1 Episódio 1", None, None);
        assert_eq!(p.kind, Kind::Episode { series: "BLEACH".into(), season: 1, episode: 1 });
        assert_eq!(p.title, "Episódio 1");
        let p = parse_name("Naruto Season 2 Episode 15 - A Luta", None, None);
        assert_eq!(p.kind, Kind::Episode { series: "Naruto".into(), season: 2, episode: 15 });
        assert_eq!(p.title, "A Luta");
        let p = parse_name("Dark T03 E08", None, None);
        assert_eq!(p.kind, Kind::Episode { series: "Dark".into(), season: 3, episode: 8 });
        // "Temporada" sozinha ou título com "ep" no meio não viram episódio
        assert_eq!(parse_name("A Temporada Dourada 2019", None, None).kind, Kind::Movie);
        assert_eq!(parse_name("Teste Eperdido 2020", None, None).kind, Kind::Movie);
    }

    #[test]
    fn episode_series_from_folders() {
        let p = parse_name("S03E01", Some("Season 3"), Some("Dark"));
        assert_eq!(p.kind, Kind::Episode { series: "Dark".into(), season: 3, episode: 1 });
        let p = parse_name("S01E01", Some("Dark (2017)"), Some("Series"));
        assert_eq!(p.kind, Kind::Episode { series: "Dark".into(), season: 1, episode: 1 });
        assert_eq!(p.year, Some(2017));
    }

    #[test]
    fn series_year_from_folder_when_file_has_none() {
        let p = parse_name("Ghosts S01E01", Some("Season 1"), Some("Ghosts (2019)"));
        assert_eq!(p.year, Some(2019));
        assert_eq!(p.kind, Kind::Episode { series: "Ghosts".into(), season: 1, episode: 1 });
        let p = parse_name("Ghosts S01E01", Some("Ghosts (2021)"), Some("Series"));
        assert_eq!(p.year, Some(2021));
        // Sem ano em lugar nenhum: continua sem ano.
        assert_eq!(parse_name("Dark S01E01", Some("Season 1"), Some("Dark")).year, None);
    }

    #[test]
    fn fansub_brackets() {
        let p = parse_name("[SubGroup] Naruto - S01E03 [1080p]", None, None);
        assert_eq!(p.kind, Kind::Episode { series: "Naruto".into(), season: 1, episode: 3 });
    }

    #[test]
    fn path_based() {
        let p = parse_path(Path::new("/media/Dark/Season 2/Dark S02E04.mkv"));
        assert_eq!(p.kind, Kind::Episode { series: "Dark".into(), season: 2, episode: 4 });
    }
}
