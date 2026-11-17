use serde::{Deserialize, Serialize};
use std::fmt::Write;

const MAX_MS: i64 = 3_600_000_000;
const MAX_ATRASO_MS: i64 = 600_000;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(
    tag = "modo",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub(in crate::api) enum Ajuste {
    Atraso {
        ms: i64,
    },
    DoisPontos {
        original_a_ms: i64,
        corrigido_a_ms: i64,
        original_b_ms: i64,
        corrigido_b_ms: i64,
    },
}

#[derive(Clone, Copy)]
pub(in crate::api) struct Mapa {
    original_ms: i64,
    corrigido_ms: i64,
    num: i64,
    den: i64,
}

impl Ajuste {
    pub(in crate::api) fn mapa(self) -> Option<Mapa> {
        match self {
            Self::Atraso { ms } if (-MAX_ATRASO_MS..=MAX_ATRASO_MS).contains(&ms) => Some(Mapa {
                original_ms: 0,
                corrigido_ms: ms,
                num: 1,
                den: 1,
            }),
            Self::DoisPontos {
                original_a_ms,
                corrigido_a_ms,
                original_b_ms,
                corrigido_b_ms,
            } if [original_a_ms, corrigido_a_ms, original_b_ms, corrigido_b_ms]
                .into_iter()
                .all(|ms| (0..=MAX_MS).contains(&ms)) =>
            {
                let den = original_b_ms - original_a_ms;
                let num = corrigido_b_ms - corrigido_a_ms;
                (den > 0 && num * 2 >= den && num <= den * 2).then_some(Mapa {
                    original_ms: original_a_ms,
                    corrigido_ms: corrigido_a_ms,
                    num,
                    den,
                })
            }
            _ => None,
        }
    }

    pub(in crate::api) fn referencias(self) -> Option<[i64; 2]> {
        match self {
            Self::Atraso { .. } => None,
            Self::DoisPontos {
                original_a_ms,
                original_b_ms,
                ..
            } => Some([original_a_ms, original_b_ms]),
        }
    }
}

impl Mapa {
    fn instante(self, ms: i64) -> i64 {
        let delta = i128::from(ms - self.original_ms);
        let proporcional = delta * i128::from(self.num) / i128::from(self.den);
        let corrigido = i128::from(self.corrigido_ms) + proporcional;
        i64::try_from(corrigido).unwrap_or(0)
    }

    fn trecho(self, trecho: &Trecho) -> Option<(i64, i64)> {
        let inicio = self.instante(trecho.inicio_ms).max(0);
        let fim = self.instante(trecho.fim_ms).min(MAX_MS);
        (fim > inicio).then_some((inicio, fim))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::api) struct Trecho {
    pub(in crate::api) inicio_ms: i64,
    fim_ms: i64,
    texto: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::api) struct Amostra<'a> {
    pub trecho: &'a Trecho,
    pub inicio_corrigido_ms: Option<i64>,
}

fn ler_tempo(tempo: &str) -> Option<i64> {
    let (relogio, ms) = tempo.split_once(',').or_else(|| tempo.split_once('.'))?;
    let mut campos = relogio.split(':');
    let hora: i64 = campos.next()?.parse().ok()?;
    let min: i64 = campos.next()?.parse().ok()?;
    let seg: i64 = campos.next()?.parse().ok()?;
    if campos.next().is_some()
        || ms.len() != 3
        || !ms.bytes().all(|b| b.is_ascii_digit())
        || !(0..=1000).contains(&hora)
        || !(0..60).contains(&min)
        || !(0..60).contains(&seg)
    {
        return None;
    }
    let ms = (hora * 3600 + min * 60 + seg) * 1000 + ms.parse::<i64>().ok()?;
    (ms <= MAX_MS).then_some(ms)
}

pub(in crate::api) fn ler(srt: &str) -> Option<Vec<Trecho>> {
    let linhas: Vec<_> = srt.trim_start_matches('\u{feff}').lines().collect();
    let mut trechos = Vec::new();
    for bloco in linhas
        .split(|linha| linha.trim().is_empty())
        .filter(|b| !b.is_empty())
    {
        let mut linhas = bloco.iter();
        let linha = linhas.next()?.trim();
        let tempo = if linha.bytes().all(|b| b.is_ascii_digit()) {
            linhas.next()?.trim()
        } else {
            linha
        };
        let (inicio, fim) = tempo.split_once("-->")?;
        let inicio_ms = ler_tempo(inicio.trim())?;
        let fim_ms = ler_tempo(fim.split_whitespace().next()?)?;
        let texto = linhas.copied().collect::<Vec<_>>().join("\n");
        if texto.is_empty() || fim_ms <= inicio_ms {
            return None;
        }
        trechos.push(Trecho {
            inicio_ms,
            fim_ms,
            texto,
        });
    }
    trechos.sort_by_key(|t| t.inicio_ms);
    (!trechos.is_empty()).then_some(trechos)
}

fn formatar(ms: i64, separador: char) -> String {
    let hora = ms / 3_600_000;
    let min = ms / 60_000 % 60;
    let seg = ms / 1000 % 60;
    let frac = ms % 1000;
    format!("{hora:02}:{min:02}:{seg:02}{separador}{frac:03}")
}

pub(in crate::api) fn renderizar(trechos: &[Trecho], mapa: Mapa, vtt: bool) -> Option<String> {
    let mut saida = if vtt {
        "WEBVTT\n\n".into()
    } else {
        String::new()
    };
    let sep = if vtt { '.' } else { ',' };
    let mut indice = 0;
    for (trecho, (inicio, fim)) in trechos
        .iter()
        .filter_map(|t| mapa.trecho(t).map(|tempos| (t, tempos)))
    {
        indice += 1;
        writeln!(
            saida,
            "{indice}\n{} --> {}\n{}\n",
            formatar(inicio, sep),
            formatar(fim, sep),
            trecho.texto,
        )
        .ok()?;
    }
    (indice != 0).then_some(saida)
}

pub(in crate::api) fn amostras(
    trechos: &[Trecho],
    referencias: Option<[i64; 2]>,
    mapa: Option<Mapa>,
) -> Vec<Amostra<'_>> {
    let selecionados = referencias.map_or_else(
        || [trechos.first(), trechos.last()],
        |refs| refs.map(|ms| trechos.iter().min_by_key(|t| t.inicio_ms.abs_diff(ms))),
    );
    selecionados
        .into_iter()
        .flatten()
        .map(|trecho| Amostra {
            trecho,
            inicio_corrigido_ms: mapa
                .and_then(|m| m.trecho(trecho))
                .map(|(inicio, _)| inicio),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atraso_descarta_trecho_que_termina_antes_do_video() {
        let trechos = ler("1\r\n00:00:01,000 --> 00:00:02,000\r\nPrimeiro\r\n\r\n2\r\n00:00:04,000 --> 00:00:06,000\r\nSegundo\r\n").unwrap();
        let mapa = Ajuste::Atraso { ms: -5000 }.mapa().unwrap();
        let corrigido = renderizar(&trechos, mapa, false).unwrap();
        assert!(!corrigido.contains("Primeiro"));
        assert!(corrigido.contains("00:00:00,000 --> 00:00:01,000\nSegundo"));
    }

    #[test]
    fn dois_pontos_ajustam_o_intervalo_e_rejeitam_ancoras_invertidas() {
        let ajuste = Ajuste::DoisPontos {
            original_a_ms: 1000,
            corrigido_a_ms: 2000,
            original_b_ms: 5000,
            corrigido_b_ms: 8000,
        };
        let mapa = ajuste.mapa().unwrap();
        assert_eq!(mapa.instante(3000), 5000);
        assert_eq!(mapa.instante(5000), 8000);
        assert!(
            Ajuste::DoisPontos {
                original_a_ms: 1000,
                corrigido_a_ms: 2000,
                original_b_ms: 1000,
                corrigido_b_ms: 0,
            }
            .mapa()
            .is_none()
        );
    }
}
