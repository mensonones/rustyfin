//! Embute nos executáveis do Windows o ícone (a logo) e as informações de versão (produto, versão, descrição, direitos).
//!
//! Sem elas, o Explorer mostra um arquivo anônimo, a assinatura de código exige um nome de produto e os antivírus
//! desconfiam mais. O recurso é montado aqui mesmo, no formato `.res`, sem depender do `rc.exe` nem do SDK do Windows:
//! o `link.exe` (MSVC) o aceita direto e, no MinGW, o `windres` o converte num objeto. Fora do Windows o script não faz nada.

use std::{env, fs, io, path::Path, path::PathBuf, process::Command};

const ICONE: &[u8] = include_bytes!("packaging/windows/rustyfin.ico");
const COPYRIGHT: &str = "Copyright (c) Rustyfin contributors. Licença MIT.";
const SITE: &str = "https://github.com/mensonones/rustyfin";
/// Idioma 0x0409 (inglês dos EUA) e página de código 0x04B0 (Unicode), o par que o Windows espera.
const IDIOMA: u16 = 0x0409;
const PAGINA_DE_CODIGO: u16 = 0x04B0;
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;
const RT_VERSION: u16 = 16;

/// Um executável do pacote: nome do binário no Cargo, descrição e nome do arquivo.
struct Binario {
    cargo: &'static str,
    descricao: &'static str,
    arquivo: &'static str,
}

const BINARIOS: [Binario; 2] = [
    Binario {
        cargo: "rustyfin",
        descricao: "Rustyfin, servidor de mídia compatível com os apps do Jellyfin",
        arquivo: "rustyfin.exe",
    },
    Binario {
        cargo: "rustyfin-tray",
        descricao: "Rustyfin, ícone da bandeja do sistema que mantém o servidor no ar",
        arquivo: "rustyfin-tray.exe",
    },
];

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=packaging/windows/rustyfin.ico");
    if !env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        return Ok(());
    }
    let versao = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let pasta = PathBuf::from(env::var("OUT_DIR").map_err(io::Error::other)?);
    let msvc = env::var("CARGO_CFG_TARGET_ENV").is_ok_and(|e| e == "msvc");

    for binario in &BINARIOS {
        let res = pasta.join(format!("{}.res", binario.cargo));
        fs::write(&res, arquivo_res(&versao, binario))?;
        // Só nos binários: os executáveis de teste não precisam do recurso.
        if msvc {
            // O `link.exe` aceita o `.res` direto.
            println!("cargo:rustc-link-arg-bin={}={}", binario.cargo, res.display());
        } else if let Some(objeto) = converter_com_windres(&res, &pasta, binario.cargo) {
            println!("cargo:rustc-link-arg-bin={}={}", binario.cargo, objeto.display());
        } else {
            println!("cargo:warning=windres não encontrado: {} fica sem ícone e sem informações de versão", binario.arquivo);
        }
    }
    Ok(())
}

/// O `ld` do MinGW não lê `.res`: o `windres` o converte num objeto.
fn converter_com_windres(res: &Path, pasta: &Path, nome: &str) -> Option<PathBuf> {
    let objeto = pasta.join(format!("{nome}.o"));
    let windres = env::var("WINDRES").unwrap_or_else(|_| "windres".into());
    Command::new(windres)
        .args(["--input-format=res", "--output-format=coff", "-i"])
        .arg(res)
        .arg("-o")
        .arg(&objeto)
        .status()
        .is_ok_and(|s| s.success())
        .then_some(objeto)
}

// ---------------------------------------------------------------- arquivo .res

/// Um `.res` começa com uma entrada vazia de 32 bytes; depois vêm os ícones, o grupo de ícones (o que o Explorer usa
/// como ícone do arquivo) e o recurso de versão.
fn arquivo_res(versao: &str, binario: &Binario) -> Vec<u8> {
    let mut res = Vec::new();
    entrada(&mut res, 0, 0, &[], 0);
    let (grupo, imagens) = icones();
    for (i, imagem) in imagens.iter().enumerate() {
        entrada(&mut res, RT_ICON, u16_de(i + 1), imagem, 0x1030);
    }
    entrada(&mut res, RT_GROUP_ICON, 1, &grupo, 0x1030);
    entrada(&mut res, RT_VERSION, 1, &versao_info(versao, binario), 0x0030);
    res
}

/// Uma entrada do `.res`: cabeçalho de 32 bytes (tipo e nome numéricos), os dados e o alinhamento em 4 bytes.
fn entrada(res: &mut Vec<u8>, tipo: u16, nome: u16, dados: &[u8], memoria: u16) {
    res.extend_from_slice(&u32_de(dados.len()).to_le_bytes());
    res.extend_from_slice(&32_u32.to_le_bytes()); // tamanho deste cabeçalho
    for valor in [0xFFFF_u16, tipo, 0xFFFF, nome] {
        res.extend_from_slice(&valor.to_le_bytes());
    }
    res.extend_from_slice(&0_u32.to_le_bytes()); // versão dos dados
    let idioma = if tipo == 0 { 0 } else { IDIOMA };
    res.extend_from_slice(&memoria.to_le_bytes());
    res.extend_from_slice(&idioma.to_le_bytes());
    res.extend_from_slice(&0_u32.to_le_bytes()); // versão
    res.extend_from_slice(&0_u32.to_le_bytes()); // características
    res.extend_from_slice(dados);
    alinhar(res);
}

// ---------------------------------------------------------------- ícones

fn le_u16(b: &[u8], desloc: usize) -> u16 {
    u16::from_le_bytes([b[desloc], b[desloc + 1]])
}

fn le_u32(b: &[u8], desloc: usize) -> u32 {
    u32::from_le_bytes([b[desloc], b[desloc + 1], b[desloc + 2], b[desloc + 3]])
}

/// Lê o `.ico` e devolve o diretório de grupo (`RT_GROUP_ICON`) e a imagem de cada tamanho (`RT_ICON`, ids 1, 2, ...).
fn icones() -> (Vec<u8>, Vec<Vec<u8>>) {
    let quantidade = usize::from(le_u16(ICONE, 4));
    let mut grupo = Vec::new();
    grupo.extend_from_slice(&0_u16.to_le_bytes());
    grupo.extend_from_slice(&1_u16.to_le_bytes()); // tipo: ícone
    grupo.extend_from_slice(&u16_de(quantidade).to_le_bytes());
    let mut imagens = Vec::new();
    for i in 0..quantidade {
        let e = 6 + 16 * i;
        let tamanho = le_u32(ICONE, e + 8) as usize;
        let desloc = le_u32(ICONE, e + 12) as usize;
        grupo.extend_from_slice(&ICONE[e..e + 8]); // largura, altura, cores, reservado, planos e bits por pixel
        grupo.extend_from_slice(&le_u32(ICONE, e + 8).to_le_bytes());
        grupo.extend_from_slice(&u16_de(i + 1).to_le_bytes());
        imagens.push(ICONE[desloc..desloc + tamanho].to_vec());
    }
    (grupo, imagens)
}

// ---------------------------------------------------------------- VS_VERSIONINFO

fn versao_info(versao: &str, binario: &Binario) -> Vec<u8> {
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
        ("FileDescription", binario.descricao),
        ("FileVersion", numeros.as_str()),
        ("InternalName", binario.cargo),
        ("LegalCopyright", COPYRIGHT),
        ("OriginalFilename", binario.arquivo),
        ("ProductName", "Rustyfin"),
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
    u16::try_from(n).expect("o valor cabe em 16 bits")
}

fn u32_de(n: usize) -> u32 {
    u32::try_from(n).expect("o valor cabe em 32 bits")
}
