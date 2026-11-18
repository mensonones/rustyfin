//! Embute no `rustyfin.exe` do Windows as informações de versão (nome do produto, versão, descrição, direitos).
//!
//! Sem elas, o Explorer mostra um arquivo anônimo, a assinatura de código exige um nome de produto e os antivírus
//! desconfiam mais. O recurso é montado aqui mesmo, no formato `.res`, sem depender do `rc.exe` nem do SDK do Windows:
//! o `link.exe` (MSVC) o aceita direto e, no MinGW, o `windres` o converte num objeto. Fora do Windows o script não faz nada.

use std::{env, fs, io, path::PathBuf, process::Command};

const PRODUTO: &str = "Rustyfin";
const DESCRICAO: &str = "Rustyfin, servidor de mídia compatível com os apps do Jellyfin";
const COPYRIGHT: &str = "Copyright (c) Rustyfin contributors. Licença MIT.";
const SITE: &str = "https://github.com/mensonones/rustyfin";
/// Idioma 0x0409 (inglês dos EUA) e página de código 0x04B0 (Unicode), o par que o Windows espera.
const IDIOMA: u16 = 0x0409;
const PAGINA_DE_CODIGO: u16 = 0x04B0;

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    if !env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        return Ok(());
    }
    let versao = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let pasta = PathBuf::from(env::var("OUT_DIR").map_err(io::Error::other)?);
    let res = pasta.join("versao.res");
    fs::write(&res, arquivo_res(&versao))?;

    // Só nos binários: os executáveis de teste não precisam do recurso.
    if env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc") {
        // O `link.exe` aceita o `.res` direto.
        println!("cargo:rustc-link-arg-bins={}", res.display());
    } else {
        // O `ld` do MinGW não lê `.res`: o `windres` o converte num objeto. Sem ele, o exe só fica sem versão.
        let objeto = pasta.join("versao.o");
        let windres = env::var("WINDRES").unwrap_or_else(|_| "windres".into());
        let convertido = Command::new(windres)
            .args(["--input-format=res", "--output-format=coff", "-i"])
            .arg(&res)
            .arg("-o")
            .arg(&objeto)
            .status()
            .is_ok_and(|s| s.success());
        if convertido {
            println!("cargo:rustc-link-arg-bins={}", objeto.display());
        } else {
            println!("cargo:warning=windres não encontrado: o rustyfin.exe fica sem as informações de versão");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- arquivo .res

/// Um `.res` começa com uma entrada vazia de 32 bytes; depois vem o recurso `RT_VERSION` (16), nome 1.
fn arquivo_res(versao: &str) -> Vec<u8> {
    let mut res = Vec::new();
    cabecalho_de_recurso(&mut res, 0, 0, 0);
    let dados = versao_info(versao);
    cabecalho_de_recurso(&mut res, u32_de(dados.len()), 16, 1);
    res.extend_from_slice(&dados);
    alinhar(&mut res);
    res
}

fn cabecalho_de_recurso(res: &mut Vec<u8>, tamanho_dos_dados: u32, tipo: u16, nome: u16) {
    res.extend_from_slice(&tamanho_dos_dados.to_le_bytes());
    res.extend_from_slice(&32_u32.to_le_bytes()); // tamanho deste cabeçalho
    if tipo == 0 && nome == 0 {
        // Entrada vazia: tipo e nome numéricos com valor zero.
        for valor in [0xFFFF_u16, 0, 0xFFFF, 0] {
            res.extend_from_slice(&valor.to_le_bytes());
        }
    } else {
        for valor in [0xFFFF_u16, tipo, 0xFFFF, nome] {
            res.extend_from_slice(&valor.to_le_bytes());
        }
    }
    res.extend_from_slice(&0_u32.to_le_bytes()); // versão dos dados
    let (memoria, idioma) = if tipo == 0 { (0_u16, 0_u16) } else { (0x0030_u16, IDIOMA) };
    res.extend_from_slice(&memoria.to_le_bytes());
    res.extend_from_slice(&idioma.to_le_bytes());
    res.extend_from_slice(&0_u32.to_le_bytes()); // versão
    res.extend_from_slice(&0_u32.to_le_bytes()); // características
}

// ---------------------------------------------------------------- VS_VERSIONINFO

fn versao_info(versao: &str) -> Vec<u8> {
    let partes: Vec<u16> = versao
        .split(['.', '-'])
        .take(4)
        .map(|p| p.parse::<u16>().unwrap_or(0))
        .chain(std::iter::repeat(0))
        .take(4)
        .collect();
    let numeros = format!("{}.{}.{}.{}", partes[0], partes[1], partes[2], partes[3]);
    let ms = (u32::from(partes[0]) << 16) | u32::from(partes[1]);
    let ls = (u32::from(partes[2]) << 16) | u32::from(partes[3]);

    // VS_FIXEDFILEINFO: 13 palavras de 32 bits (52 bytes).
    let mut fixa = Vec::new();
    for palavra in [0xFEEF_04BD_u32, 0x0001_0000, ms, ls, ms, ls, 0x3F, 0, 0x0004_0004, 1, 0, 0, 0] {
        fixa.extend_from_slice(&palavra.to_le_bytes());
    }

    let textos = [
        ("CompanyName", "Rustyfin contributors"),
        ("FileDescription", DESCRICAO),
        ("FileVersion", numeros.as_str()),
        ("InternalName", "rustyfin"),
        ("LegalCopyright", COPYRIGHT),
        ("OriginalFilename", "rustyfin.exe"),
        ("ProductName", PRODUTO),
        ("ProductVersion", versao),
        ("Comments", SITE),
    ];
    let strings: Vec<Vec<u8>> = textos.iter().map(|(chave, valor)| no_de_texto(chave, valor)).collect();
    let tabela = no(&format!("{IDIOMA:04X}{PAGINA_DE_CODIGO:04X}"), 0, 1, &[], &strings);
    let string_file_info = no("StringFileInfo", 0, 1, &[], &[tabela]);

    let mut traducao = Vec::new();
    traducao.extend_from_slice(&IDIOMA.to_le_bytes());
    traducao.extend_from_slice(&PAGINA_DE_CODIGO.to_le_bytes());
    let var = no("Translation", 4, 0, &traducao, &[]);
    let var_file_info = no("VarFileInfo", 0, 1, &[], &[var]);

    no("VS_VERSION_INFO", u16_de(fixa.len()), 0, &fixa, &[string_file_info, var_file_info])
}

/// Nó de texto (`String`): o tamanho do valor é contado em caracteres, com o zero final.
fn no_de_texto(chave: &str, valor: &str) -> Vec<u8> {
    let mut texto = utf16(valor);
    texto.extend_from_slice(&0_u16.to_le_bytes());
    no(chave, u16_de(texto.len() / 2), 1, &texto, &[])
}

/// Nó genérico do VERSIONINFO: `wLength`, `wValueLength`, `wType`, chave (UTF-16, zero final), valor e filhos, cada
/// parte alinhada em 4 bytes a partir do início do nó.
fn no(chave: &str, tamanho_do_valor: u16, tipo: u16, valor: &[u8], filhos: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0, 0]; // wLength, preenchido no fim
    b.extend_from_slice(&tamanho_do_valor.to_le_bytes());
    b.extend_from_slice(&tipo.to_le_bytes());
    b.extend_from_slice(&utf16(chave));
    b.extend_from_slice(&0_u16.to_le_bytes());
    alinhar(&mut b);
    b.extend_from_slice(valor);
    for filho in filhos {
        alinhar(&mut b);
        b.extend_from_slice(filho);
    }
    let tamanho = u16_de(b.len()).to_le_bytes();
    b[0] = tamanho[0];
    b[1] = tamanho[1];
    b
}

fn utf16(texto: &str) -> Vec<u8> {
    texto.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn alinhar(b: &mut Vec<u8>) {
    while b.len() % 4 != 0 {
        b.push(0);
    }
}

fn u16_de(n: usize) -> u16 {
    u16::try_from(n).expect("o recurso de versão cabe em 64 KiB")
}

fn u32_de(n: usize) -> u32 {
    u32::try_from(n).expect("o recurso de versão cabe em 4 GiB")
}
