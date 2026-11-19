//! Capítulos do arquivo (lidos pelo ffprobe) e os segmentos que eles indicam: abertura, créditos, resumo e prévia.
//!
//! Os apps do Jellyfin com suporte a segmentos (como o Android TV 0.18) mostram o botão de pular a partir de
//! `GET /MediaSegments/{id}`. Aqui os segmentos vêm dos nomes dos capítulos, como o plugin "Chapter Segments Provider"
//! do Jellyfin faz; a detecção pelo áudio, para os arquivos sem capítulos, é uma etapa futura.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Um capítulo: o nome e o instante de início, em ticks (1 s = 10.000.000).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capitulo {
    #[serde(rename = "n")]
    pub nome: String,
    #[serde(rename = "t")]
    pub inicio_ticks: i64,
}

/// O que o capítulo indica, com os nomes que a API do Jellyfin usa.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TipoSegmento {
    Intro,
    Outro,
    Recap,
    Preview,
}

impl TipoSegmento {
    pub fn nome(self) -> &'static str {
        match self {
            TipoSegmento::Intro => "Intro",
            TipoSegmento::Outro => "Outro",
            TipoSegmento::Recap => "Recap",
            TipoSegmento::Preview => "Preview",
        }
    }
}

/// Um trecho a pular, em ticks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segmento {
    pub tipo: TipoSegmento,
    pub inicio_ticks: i64,
    pub fim_ticks: i64,
}

/// Lê `chapters` da saída JSON do `ffprobe -show_chapters`. Capítulo sem título vira "Capítulo N".
pub fn ler(ffprobe: &Value) -> Vec<Capitulo> {
    let Some(lista) = ffprobe.get("chapters").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut capitulos: Vec<Capitulo> = lista
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let segundos = c.get("start_time").and_then(Value::as_str)?.parse::<f64>().ok()?;
            if !segundos.is_finite() || segundos < 0.0 {
                return None;
            }
            let nome = c
                .pointer("/tags/title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map_or_else(|| format!("Capítulo {}", i + 1), ToOwned::to_owned);
            Some(Capitulo { nome, inicio_ticks: (segundos * 10_000_000.0).round() as i64 })
        })
        .collect();
    capitulos.sort_by_key(|c| c.inicio_ticks);
    capitulos
}

/// Minúsculas, sem acentos comuns e sem pontuação nas pontas: "Abertura", "ABERTURA!" e "abertura" dão o mesmo.
fn normalizar(nome: &str) -> String {
    nome.trim()
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'á' | 'à' | 'â' | 'ã' | 'ä' => 'a',
            'é' | 'è' | 'ê' | 'ë' => 'e',
            'í' | 'ì' | 'î' | 'ï' => 'i',
            'ó' | 'ò' | 'ô' | 'õ' | 'ö' => 'o',
            'ú' | 'ù' | 'û' | 'ü' => 'u',
            'ç' => 'c',
            outro => outro,
        })
        .collect::<String>()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_owned()
}

/// `OP`, `OP1`, `ED2`: abreviações usadas em animes e em releases de fansub.
fn abreviacao(texto: &str, prefixo: &str) -> bool {
    texto
        .strip_prefix(prefixo)
        .is_some_and(|resto| resto.chars().all(|c| c.is_ascii_digit()))
}

/// O que o nome de um capítulo indica, se indicar algo. Só vale a palavra inteira, ela com um número depois ("Opening 1")
/// ou a frase conhecida: "Introduction to the Dragon" não é uma abertura.
pub fn tipo_do_capitulo(nome: &str) -> Option<TipoSegmento> {
    let n = normalizar(nome);
    // A palavra sozinha ou seguida só de um número ("Intro 2", "Opening - 1").
    let casa = |palavra: &str| {
        n == palavra
            || n.strip_prefix(palavra)
                .is_some_and(|r| r.trim_matches(|c: char| !c.is_alphanumeric()).chars().all(|c| c.is_ascii_digit()))
    };
    if ["intro", "opening", "abertura", "main title"].iter().any(|p| casa(p)) || abreviacao(&n, "op") {
        return Some(TipoSegmento::Intro);
    }
    if ["outro", "ending", "credits", "creditos", "encerramento", "end credits", "closing"].iter().any(|p| casa(p))
        || abreviacao(&n, "ed")
    {
        return Some(TipoSegmento::Outro);
    }
    if ["recap", "previously", "previously on", "anteriormente", "resumo"].iter().any(|p| casa(p)) {
        return Some(TipoSegmento::Recap);
    }
    if ["preview", "next time", "proximo episodio", "proximo", "previa", "teaser"].iter().any(|p| casa(p)) {
        return Some(TipoSegmento::Preview);
    }
    None
}

/// Os segmentos indicados pelos capítulos: cada um vai do início do capítulo até o início do seguinte (ou o fim do vídeo).
/// Capítulos consecutivos do mesmo tipo (`OP1`, `OP2`) formam um só segmento.
pub fn segmentos(capitulos: &[Capitulo], duracao_ticks: i64) -> Vec<Segmento> {
    let mut saida: Vec<Segmento> = Vec::new();
    for (i, c) in capitulos.iter().enumerate() {
        let Some(tipo) = tipo_do_capitulo(&c.nome) else {
            continue;
        };
        let fim = capitulos.get(i + 1).map_or(duracao_ticks, |p| p.inicio_ticks);
        if fim <= c.inicio_ticks {
            continue;
        }
        match saida.last_mut() {
            Some(anterior) if anterior.tipo == tipo && anterior.fim_ticks == c.inicio_ticks => {
                anterior.fim_ticks = fim;
            }
            _ => saida.push(Segmento { tipo, inicio_ticks: c.inicio_ticks, fim_ticks: fim }),
        }
    }
    saida
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cap(nome: &str, segundos: i64) -> Capitulo {
        Capitulo { nome: nome.to_owned(), inicio_ticks: segundos * 10_000_000 }
    }

    #[test]
    fn le_os_capitulos_do_ffprobe_com_e_sem_titulo() {
        let saida = json!({ "chapters": [
            { "id": 1, "start_time": "90.500000", "tags": { "title": " Abertura " } },
            { "id": 0, "start_time": "0.000000" },
            { "id": 2, "start_time": "invalido", "tags": { "title": "x" } },
        ] });
        let c = ler(&saida);
        assert_eq!(c, [cap("Capítulo 2", 0), Capitulo { nome: "Abertura".into(), inicio_ticks: 905_000_000 }]);
        assert!(ler(&json!({})).is_empty());
    }

    #[test]
    fn reconhece_os_nomes_de_abertura_creditos_resumo_e_previa() {
        for (nome, tipo) in [
            ("Intro", TipoSegmento::Intro), ("Opening", TipoSegmento::Intro), ("Abertura!", TipoSegmento::Intro),
            ("OP", TipoSegmento::Intro), ("op2", TipoSegmento::Intro), ("Opening 1", TipoSegmento::Intro),
            ("Credits", TipoSegmento::Outro), ("Créditos", TipoSegmento::Outro), ("ED1", TipoSegmento::Outro),
            ("Ending", TipoSegmento::Outro), ("End Credits", TipoSegmento::Outro),
            ("Recap", TipoSegmento::Recap), ("Previously", TipoSegmento::Recap),
            ("Preview", TipoSegmento::Preview), ("Next Time", TipoSegmento::Preview),
        ] {
            assert_eq!(tipo_do_capitulo(nome), Some(tipo), "{nome}");
        }
        // Frases que só começam com a palavra não contam.
        for nome in ["Introduction to the Dragon", "The Opening of the Gate", "Capítulo 3", "Cena final", "Operation", "Edward"] {
            assert_eq!(tipo_do_capitulo(nome), None, "{nome}");
        }
    }

    #[test]
    fn segmentos_vao_ate_o_proximo_capitulo_e_juntam_os_consecutivos_do_mesmo_tipo() {
        let capitulos = [
            cap("Recap", 0), cap("Abertura", 60), cap("Capítulo 3", 150), cap("Cena", 900),
            cap("ED1", 1300), cap("ED2", 1330), cap("Preview", 1390),
        ];
        let s = segmentos(&capitulos, 1400 * 10_000_000);
        assert_eq!(s, [
            Segmento { tipo: TipoSegmento::Recap, inicio_ticks: 0, fim_ticks: 600_000_000 },
            Segmento { tipo: TipoSegmento::Intro, inicio_ticks: 600_000_000, fim_ticks: 1_500_000_000 },
            Segmento { tipo: TipoSegmento::Outro, inicio_ticks: 13_000_000_000, fim_ticks: 13_900_000_000 },
            Segmento { tipo: TipoSegmento::Preview, inicio_ticks: 13_900_000_000, fim_ticks: 14_000_000_000 },
        ]);
        assert!(segmentos(&[cap("Capítulo 1", 0), cap("Capítulo 2", 600)], 1_000_000_000).is_empty());
        // Capítulo no fim do vídeo, sem duração depois dele, não vira segmento vazio.
        assert!(segmentos(&[cap("Credits", 100)], 1_000_000_000).is_empty());
    }
}
