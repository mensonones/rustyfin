//! Preferências de reprodução por usuário (idioma do áudio e da legenda) e a escolha das faixas padrão que o servidor
//! indica aos apps em `DefaultAudioStreamIndex` e `DefaultSubtitleStreamIndex`, como o Jellyfin faz.

use crate::models::MediaStream;
use serde::Serialize;
use serde_json::Value;

/// Como a legenda é escolhida (os mesmos nomes que a API do Jellyfin usa em `SubtitleMode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub enum ModoLegenda {
    /// A faixa que o arquivo marca como padrão ou forçada: o servidor não indica nada.
    #[default]
    Default,
    /// Legenda no idioma preferido só quando o áudio está em outro idioma.
    Smart,
    /// Só as legendas forçadas (falas em outro idioma).
    OnlyForced,
    /// Sempre uma legenda, de preferência no idioma preferido.
    Always,
    /// Nunca.
    Nenhuma,
}

impl ModoLegenda {
    pub fn nome(self) -> &'static str {
        match self {
            ModoLegenda::Default => "Default",
            ModoLegenda::Smart => "Smart",
            ModoLegenda::OnlyForced => "OnlyForced",
            ModoLegenda::Always => "Always",
            ModoLegenda::Nenhuma => "None",
        }
    }

    pub fn de_nome(nome: &str) -> Option<Self> {
        match nome.trim().to_lowercase().as_str() {
            "default" => Some(ModoLegenda::Default),
            "smart" => Some(ModoLegenda::Smart),
            "onlyforced" => Some(ModoLegenda::OnlyForced),
            "always" => Some(ModoLegenda::Always),
            "none" => Some(ModoLegenda::Nenhuma),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Preferencias {
    /// Idioma preferido do áudio (por exemplo `por`, `jpn`); `None` = o do arquivo.
    pub idioma_audio: Option<String>,
    /// Tocar a faixa de áudio padrão do arquivo mesmo que o idioma preferido exista (como a opção do Jellyfin). Ligada por
    /// padrão: o idioma só vale com ela desligada.
    pub tocar_faixa_padrao: bool,
    pub idioma_legenda: Option<String>,
    pub modo_legenda: ModoLegenda,
}

impl Default for Preferencias {
    fn default() -> Self {
        Preferencias { idioma_audio: None, tocar_faixa_padrao: true, idioma_legenda: None, modo_legenda: ModoLegenda::Default }
    }
}

impl Preferencias {
    /// Sem nenhuma escolha do usuário: o servidor pode aplicar os padrões do tipo de biblioteca (anime).
    pub fn e_padrao(&self) -> bool {
        self.idioma_audio.is_none() && self.idioma_legenda.is_none() && self.modo_legenda == ModoLegenda::Default
    }

    /// Anime: áudio em japonês e, se o áudio não é português, legenda em português.
    pub fn padrao_de_anime() -> Self {
        Preferencias {
            idioma_audio: Some("jpn".into()),
            tocar_faixa_padrao: false,
            idioma_legenda: Some("por".into()),
            modo_legenda: ModoLegenda::Smart,
        }
    }
}

const GRUPOS: &[&[&str]] = &[
    &["por", "pt", "pob", "ptbr", "pt-br", "pt_br", "portuguese"],
    &["eng", "en", "english"],
    &["jpn", "ja", "jp", "jap", "japanese"],
    &["spa", "es", "esp", "spanish"],
    &["fre", "fra", "fr", "french"],
    &["ger", "deu", "de", "german"],
    &["ita", "it", "italian"],
    &["kor", "ko", "korean"],
    &["zho", "chi", "zh", "chs", "cht", "chinese"],
    &["rus", "ru", "russian"],
];

fn canonico(codigo: &str) -> String {
    let c = codigo.trim().to_lowercase();
    GRUPOS.iter().find(|g| g.contains(&c.as_str())).map_or(c, |g| g[0].to_owned())
}

/// Dois códigos de idioma (ISO 639-1, 639-2 ou o nome em inglês) são o mesmo idioma?
pub fn mesmo_idioma(a: &str, b: &str) -> bool {
    !a.trim().is_empty() && canonico(a) == canonico(b)
}

fn idioma_da_faixa(f: &MediaStream, idioma: &str) -> bool {
    f.language.as_deref().is_some_and(|l| mesmo_idioma(l, idioma))
}

/// O texto não é um código de idioma.
#[derive(Debug, PartialEq, Eq)]
pub struct IdiomaInvalido;

/// Valida um código de idioma digitado: 2 a 3 letras, com região opcional (`pt-BR`). Vazio vira `None`.
pub fn validar_idioma(texto: &str) -> Result<Option<String>, IdiomaInvalido> {
    let t = texto.trim().to_lowercase();
    if t.is_empty() {
        return Ok(None);
    }
    let (base, regiao) = t.split_once(['-', '_']).map_or((t.as_str(), None), |(b, r)| (b, Some(r)));
    let letras = |s: &str, min: usize, max: usize| (min..=max).contains(&s.len()) && s.chars().all(|c| c.is_ascii_lowercase());
    if letras(base, 2, 3) && regiao.is_none_or(|r| letras(r, 2, 4)) {
        Ok(Some(t))
    } else {
        Err(IdiomaInvalido)
    }
}

/// Aplica os campos presentes no JSON (as chaves vêm de quem chama: `PascalCase` na API do Jellyfin, `camelCase` no painel) sobre
/// as preferências atuais. Campo ausente não muda; `null` ou texto vazio limpa o idioma. Valor inválido devolve o erro.
pub fn aplicar_json(
    atual: &Preferencias,
    corpo: &Value,
    chaves: [&str; 4], // idioma do áudio, faixa padrão, idioma da legenda, modo
) -> Result<Preferencias, &'static str> {
    let mut p = atual.clone();
    let texto = |chave: &str| corpo.get(chave).map(|v| v.as_str().unwrap_or(""));
    if let Some(t) = texto(chaves[0]) {
        p.idioma_audio = validar_idioma(t).map_err(|IdiomaInvalido| "Idioma de áudio inválido (use códigos como pt, por ou pt-BR).")?;
    }
    if let Some(b) = corpo.get(chaves[1]).and_then(Value::as_bool) {
        p.tocar_faixa_padrao = b;
    }
    if let Some(t) = texto(chaves[2]) {
        p.idioma_legenda = validar_idioma(t).map_err(|IdiomaInvalido| "Idioma de legenda inválido (use códigos como pt, por ou pt-BR).")?;
    }
    if let Some(m) = corpo.get(chaves[3]).and_then(Value::as_str) {
        p.modo_legenda = ModoLegenda::de_nome(m).ok_or("Modo de legenda inválido (Default, Smart, OnlyForced, Always ou None).")?;
    }
    Ok(p)
}

/// As faixas que o app deve escolher sozinho: `(áudio, legenda)`. `None` = o servidor não indica (o app segue o arquivo) e
/// `Some(-1)` na legenda = nenhuma legenda.
pub fn escolher_faixas(faixas: &[MediaStream], prefs: &Preferencias) -> (Option<i32>, Option<i32>) {
    let audios: Vec<&MediaStream> = faixas.iter().filter(|f| f.r#type == "Audio").collect();
    let legendas: Vec<&MediaStream> = faixas.iter().filter(|f| f.r#type == "Subtitle").collect();

    let audio = match (&prefs.idioma_audio, prefs.tocar_faixa_padrao) {
        (Some(idioma), false) => audios.iter().find(|f| idioma_da_faixa(f, idioma)).map(|f| f.index),
        _ => None,
    };
    // O idioma do áudio que vai tocar: o escolhido, senão o padrão do arquivo, senão o primeiro.
    let audio_em_uso = audio
        .and_then(|i| audios.iter().find(|f| f.index == i))
        .or_else(|| audios.iter().find(|f| f.is_default))
        .or_else(|| audios.first());

    let no_idioma = |f: &&&MediaStream| prefs.idioma_legenda.as_deref().is_none_or(|l| idioma_da_faixa(f, l));
    let completa = |f: &&&MediaStream| !f.is_forced;
    let legenda = match prefs.modo_legenda {
        ModoLegenda::Default => None,
        ModoLegenda::Nenhuma => Some(-1),
        ModoLegenda::Always => legendas
            .iter()
            .filter(no_idioma)
            .find(completa)
            .or_else(|| legendas.iter().find(no_idioma))
            .map(|f| f.index),
        ModoLegenda::Smart => match &prefs.idioma_legenda {
            None => None,
            Some(idioma) => {
                let audio_ja_no_idioma = audio_em_uso.is_some_and(|f| idioma_da_faixa(f, idioma));
                if audio_ja_no_idioma {
                    Some(-1)
                } else {
                    legendas
                        .iter()
                        .filter(no_idioma)
                        .find(completa)
                        .or_else(|| legendas.iter().find(no_idioma))
                        .map(|f| f.index)
                }
            }
        },
        ModoLegenda::OnlyForced => Some(
            legendas
                .iter()
                .filter(|f| f.is_forced)
                .find(no_idioma)
                .or_else(|| legendas.iter().find(|f| f.is_forced))
                .map_or(-1, |f| f.index),
        ),
    };
    (audio, legenda)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn faixa(tipo: &str, index: i32, idioma: &str, padrao: bool, forcada: bool) -> MediaStream {
        MediaStream {
            language: Some(idioma.to_string()),
            is_default: padrao,
            is_forced: forcada,
            ..MediaStream::basic("x", tipo, index)
        }
    }

    fn arquivo() -> Vec<MediaStream> {
        vec![
            faixa("Video", 0, "und", true, false),
            faixa("Audio", 1, "eng", true, false),
            faixa("Audio", 2, "por", false, false),
            faixa("Audio", 3, "jpn", false, false),
            faixa("Subtitle", 4, "eng", false, false),
            faixa("Subtitle", 5, "por", false, true),
            faixa("Subtitle", 6, "por", false, false),
        ]
    }

    fn prefs(audio: Option<&str>, padrao: bool, legenda: Option<&str>, modo: ModoLegenda) -> Preferencias {
        Preferencias { idioma_audio: audio.map(Into::into), tocar_faixa_padrao: padrao, idioma_legenda: legenda.map(Into::into), modo_legenda: modo }
    }

    #[test]
    fn idiomas_sao_comparados_por_grupo_e_sem_caixa() {
        assert!(mesmo_idioma("pt-BR", "por") && mesmo_idioma("POB", "pt") && mesmo_idioma("ja", "jpn"));
        assert!(!mesmo_idioma("por", "eng") && !mesmo_idioma("", ""));
        assert!(mesmo_idioma("xyz", "XYZ"), "códigos desconhecidos comparam literalmente");
        assert_eq!(validar_idioma(" pt-BR "), Ok(Some("pt-br".into())));
        assert_eq!(validar_idioma(""), Ok(None));
        assert!(validar_idioma("portugues").is_err() && validar_idioma("p").is_err() && validar_idioma("pt-1").is_err());
    }

    #[test]
    fn audio_pelo_idioma_so_com_a_faixa_padrao_desligada() {
        let f = arquivo();
        assert_eq!(escolher_faixas(&f, &prefs(Some("por"), false, None, ModoLegenda::Default)).0, Some(2));
        // Com "tocar a faixa padrão" ligada (o padrão do Jellyfin), o idioma não manda.
        assert_eq!(escolher_faixas(&f, &prefs(Some("por"), true, None, ModoLegenda::Default)).0, None);
        // Idioma que o arquivo não tem: o app segue o arquivo.
        assert_eq!(escolher_faixas(&f, &prefs(Some("fra"), false, None, ModoLegenda::Default)).0, None);
    }

    #[test]
    fn modos_de_legenda() {
        let f = arquivo();
        let p = |modo| escolher_faixas(&f, &prefs(None, true, Some("por"), modo)).1;
        assert_eq!(p(ModoLegenda::Default), None);
        assert_eq!(p(ModoLegenda::Nenhuma), Some(-1));
        // Sempre: a completa em português (a forçada só se não houver outra).
        assert_eq!(p(ModoLegenda::Always), Some(6));
        // Só forçadas: a forçada em português.
        assert_eq!(p(ModoLegenda::OnlyForced), Some(5));
        // Inteligente com áudio padrão em inglês: legenda em português.
        assert_eq!(p(ModoLegenda::Smart), Some(6));
        // Inteligente com o áudio já em português: nenhuma legenda.
        let so_por = escolher_faixas(&f, &prefs(Some("por"), false, Some("por"), ModoLegenda::Smart));
        assert_eq!(so_por, (Some(2), Some(-1)));
        // Só forçadas sem nenhuma forçada no arquivo: nenhuma legenda.
        let sem_forcada: Vec<_> = f.iter().filter(|x| !x.is_forced).cloned().collect();
        assert_eq!(escolher_faixas(&sem_forcada, &prefs(None, true, Some("por"), ModoLegenda::OnlyForced)).1, Some(-1));
        // Sempre, sem legenda no idioma: o app decide.
        assert_eq!(escolher_faixas(&f, &prefs(None, true, Some("fra"), ModoLegenda::Always)).1, None);
    }

    #[test]
    fn anime_e_o_modo_inteligente_com_japones_e_portugues() {
        let f = arquivo();
        assert_eq!(escolher_faixas(&f, &Preferencias::padrao_de_anime()), (Some(3), Some(6)));
        // Sem áudio japonês e com o áudio padrão já em português: sem legenda.
        let dublado = [faixa("Audio", 1, "por", true, false), faixa("Subtitle", 2, "por", false, false)];
        assert_eq!(escolher_faixas(&dublado, &Preferencias::padrao_de_anime()), (None, Some(-1)));
        // Áudio japonês sem legenda em português: só o áudio.
        let sem_pt = [faixa("Audio", 1, "jpn", true, false), faixa("Subtitle", 2, "eng", false, false)];
        assert_eq!(escolher_faixas(&sem_pt, &Preferencias::padrao_de_anime()), (Some(1), None));
        assert_eq!(escolher_faixas(&[], &Preferencias::padrao_de_anime()), (None, None));
        assert!(Preferencias::default().e_padrao() && !Preferencias::padrao_de_anime().e_padrao());
    }

    #[test]
    fn json_aplica_so_o_que_veio_e_rejeita_o_invalido() {
        let atual = Preferencias::default();
        let chaves = ["AudioLanguagePreference", "PlayDefaultAudioTrack", "SubtitleLanguagePreference", "SubtitleMode"];
        let novo = aplicar_json(
            &atual,
            &json!({ "AudioLanguagePreference": "pt-BR", "PlayDefaultAudioTrack": false, "SubtitleMode": "smart", "Outro": 1 }),
            chaves,
        )
        .unwrap();
        assert_eq!(novo, prefs(Some("pt-br"), false, None, ModoLegenda::Smart));
        // null e texto vazio limpam o idioma; o que não veio não muda.
        let limpo = aplicar_json(&novo, &json!({ "AudioLanguagePreference": null }), chaves).unwrap();
        assert_eq!(limpo, prefs(None, false, None, ModoLegenda::Smart));
        assert!(aplicar_json(&atual, &json!({ "SubtitleMode": "Sempre" }), chaves).is_err());
        assert!(aplicar_json(&atual, &json!({ "SubtitleLanguagePreference": "portugues" }), chaves).is_err());
    }
}
