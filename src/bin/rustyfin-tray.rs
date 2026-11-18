//! Ícone do Rustyfin na bandeja do sistema (Windows).
//!
//! Inicia o `rustyfin.exe` sem janela, na pasta onde este programa está, mantém o servidor no ar (se ele cair, sobe de
//! novo) e oferece um menu: abrir o painel, ver o log, abrir a pasta de dados, reiniciar e sair. É o que o setup e a
//! tarefa agendada executam. A configuração vem do `rustyfin.env` da mesma pasta (formato `CHAVE=VALOR`).
//!
//! Fora do Windows não há bandeja: rode o `rustyfin` direto.

#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(not(windows))]
fn main() {
    eprintln!("O ícone da bandeja existe só no Windows. Nos outros sistemas, rode o `rustyfin` direto.");
}

#[cfg(windows)]
fn main() {
    bandeja::executar();
}

#[cfg(windows)]
mod bandeja {
    use std::{
        collections::HashMap,
        fs::{self, OpenOptions},
        io::{Read, Write},
        net::{SocketAddr, TcpListener, TcpStream},
        os::windows::process::CommandExt,
        path::Path,
        process::{Child, Command, Stdio},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tray_icon::{
        Icon, TrayIconBuilder,
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    };

    /// `CREATE_NO_WINDOW`: o servidor e os comandos auxiliares não abrem janela de console.
    const SEM_JANELA: u32 = 0x0800_0000;
    /// Porta local que só serve de trava: dois ícones na bandeja disputariam o mesmo servidor.
    const PORTA_DA_TRAVA: u16 = 47_359;
    const PORTA_PADRAO: u16 = 8096;
    /// Quanto esperar o servidor encerrar sozinho antes de forçar.
    const PRAZO_DO_DESLIGAMENTO: Duration = Duration::from_secs(15);

    /// Sinais entre a bandeja (thread principal) e o supervisor do servidor.
    #[derive(Default)]
    struct Estado {
        /// O usuário pediu para sair: o servidor é desligado e o supervisor termina.
        sair: AtomicBool,
        /// O usuário pediu para reiniciar: o servidor é desligado e sobe de novo.
        reiniciar: AtomicBool,
        /// O desligamento gracioso não funcionou a tempo: encerra o processo do servidor.
        matar: AtomicBool,
        /// O servidor está de pé.
        rodando: AtomicBool,
        /// O supervisor terminou (o programa pode fechar).
        parado: AtomicBool,
    }

    pub fn executar() {
        // Uma instância só: se a porta da trava estiver ocupada, já há um ícone do Rustyfin na bandeja.
        let Ok(trava) = TcpListener::bind(("127.0.0.1", PORTA_DA_TRAVA)) else {
            return;
        };
        let Some(pasta) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) else {
            return;
        };
        let configuracao = ler_configuracao(&pasta.join("rustyfin.env"));
        let porta = configuracao
            .get("RUSTYFIN_PORT")
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(PORTA_PADRAO);
        let estado = Arc::new(Estado::default());

        {
            let (estado, pasta, configuracao) = (estado.clone(), pasta.clone(), configuracao.clone());
            thread::spawn(move || supervisor(&estado, &pasta, &configuracao));
        }

        let evento = EventLoopBuilder::new().build();
        let abrir_painel = MenuItem::new("Abrir o painel", true, None);
        let ver_log = MenuItem::new("Ver o log", true, None);
        let abrir_pasta = MenuItem::new("Abrir a pasta de dados", true, None);
        let reiniciar = MenuItem::new("Reiniciar o servidor", true, None);
        let sair = MenuItem::new("Sair", true, None);
        let menu = Menu::new();
        let montado = menu.append_items(&[
            &abrir_painel,
            &ver_log,
            &abrir_pasta,
            &PredefinedMenuItem::separator(),
            &reiniciar,
            &sair,
        ]);
        let icone = Icon::from_resource(1, Some((32, 32)));
        let (Ok(()), Ok(icone)) = (montado, icone) else {
            registrar(&pasta, "Não foi possível montar o menu da bandeja; o servidor segue sem o ícone.");
            return sem_bandeja(&estado);
        };
        let Ok(bandeja) = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(format!("Rustyfin (porta {porta})"))
            .with_icon(icone)
            .build()
        else {
            // Sem área de notificação (por exemplo, sessão sem desktop): o servidor continua no ar, sem o ícone.
            registrar(&pasta, "Não foi possível criar o ícone na bandeja; o servidor segue sem ele.");
            return sem_bandeja(&estado);
        };

        let url = format!("http://localhost:{porta}/admin");
        let mut bandeja = Some(bandeja);
        let mut ultimo_texto = String::new();
        evento.run(move |_, _, fluxo| {
            // A trava precisa viver até o fim do programa.
            let _ = &trava;
            *fluxo = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(250));

            while let Ok(escolha) = MenuEvent::receiver().try_recv() {
                if escolha.id() == abrir_painel.id() {
                    abrir(&url);
                } else if escolha.id() == ver_log.id() {
                    abrir(&pasta.join("rustyfin.log").to_string_lossy());
                } else if escolha.id() == abrir_pasta.id() {
                    abrir(&pasta.join("dados").to_string_lossy());
                } else if escolha.id() == reiniciar.id() {
                    estado.reiniciar.store(true, Ordering::SeqCst);
                    desligar_em_segundo_plano(&estado, porta, &configuracao);
                } else if escolha.id() == sair.id() {
                    estado.sair.store(true, Ordering::SeqCst);
                    desligar_em_segundo_plano(&estado, porta, &configuracao);
                }
            }

            let texto = if estado.sair.load(Ordering::SeqCst) {
                "Rustyfin: encerrando...".to_owned()
            } else if estado.rodando.load(Ordering::SeqCst) {
                format!("Rustyfin: rodando na porta {porta}")
            } else {
                "Rustyfin: iniciando...".to_owned()
            };
            if texto != ultimo_texto {
                if let Some(b) = &bandeja {
                    let _ = b.set_tooltip(Some(&texto));
                }
                ultimo_texto = texto;
            }

            if estado.parado.load(Ordering::SeqCst) {
                // Descarta o ícone antes de sair, para ele não ficar "fantasma" na bandeja.
                bandeja = None;
                *fluxo = ControlFlow::Exit;
            }
        });
    }

    /// Sem ícone, o programa só espera o supervisor terminar (o servidor continua no ar).
    fn sem_bandeja(estado: &Estado) {
        while !estado.parado.load(Ordering::SeqCst) {
            thread::sleep(Duration::from_millis(500));
        }
    }

    // ---------------------------------------------------------------- configuração

    /// Lê o `rustyfin.env` (`CHAVE=VALOR`, `#` para comentários, aspas opcionais).
    fn ler_configuracao(arquivo: &Path) -> HashMap<String, String> {
        let mut mapa = HashMap::new();
        let Ok(texto) = fs::read_to_string(arquivo) else {
            return mapa;
        };
        for linha in texto.trim_start_matches('\u{feff}').lines() {
            let linha = linha.trim();
            if linha.is_empty() || linha.starts_with('#') {
                continue;
            }
            if let Some((chave, valor)) = linha.split_once('=') {
                mapa.insert(chave.trim().to_owned(), valor.trim().trim_matches('"').to_owned());
            }
        }
        mapa
    }

    // ---------------------------------------------------------------- supervisor do servidor

    fn registrar(pasta: &Path, texto: &str) {
        if let Ok(mut log) = OpenOptions::new().create(true).append(true).open(pasta.join("rustyfin.log")) {
            let _ = writeln!(log, "{texto}");
        }
    }

    fn iniciar_servidor(pasta: &Path, configuracao: &HashMap<String, String>) -> std::io::Result<Child> {
        let log = OpenOptions::new().create(true).append(true).open(pasta.join("rustyfin.log"))?;
        let log_de_erro = log.try_clone()?;
        let mut comando = Command::new(pasta.join("rustyfin.exe"));
        comando
            .current_dir(pasta)
            .envs(configuracao)
            .env("NO_COLOR", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_de_erro))
            .creation_flags(SEM_JANELA);
        if !configuracao.contains_key("RUSTYFIN_DATA_DIR") {
            comando.env("RUSTYFIN_DATA_DIR", pasta.join("dados"));
        }
        comando.spawn()
    }

    /// Mantém o servidor no ar: se ele cair, sobe de novo (com espera crescente se cair logo ao iniciar).
    fn supervisor(estado: &Estado, pasta: &Path, configuracao: &HashMap<String, String>) {
        let mut espera = Duration::from_secs(5);
        while !estado.sair.load(Ordering::SeqCst) {
            let inicio = Instant::now();
            let mut filho = match iniciar_servidor(pasta, configuracao) {
                Ok(f) => f,
                Err(erro) => {
                    registrar(pasta, &format!("Não foi possível iniciar o rustyfin.exe: {erro}"));
                    thread::sleep(espera);
                    espera = (espera * 2).min(Duration::from_secs(60));
                    continue;
                }
            };
            estado.rodando.store(true, Ordering::SeqCst);
            let saida = loop {
                if estado.matar.load(Ordering::SeqCst) {
                    let _ = filho.kill();
                }
                match filho.try_wait() {
                    Ok(Some(status)) => break Some(status),
                    Ok(None) => thread::sleep(Duration::from_millis(300)),
                    Err(_) => break None,
                }
            };
            estado.rodando.store(false, Ordering::SeqCst);
            estado.matar.store(false, Ordering::SeqCst);

            let normal = saida.is_some_and(|s| s.success());
            if estado.reiniciar.swap(false, Ordering::SeqCst) {
                registrar(pasta, "Servidor reiniciado pelo ícone da bandeja.");
                espera = Duration::from_secs(5);
            } else if normal {
                // Terminou sem erro e sem ninguém pedir: alguém desligou o servidor de propósito.
                break;
            } else {
                let codigo = saida.and_then(|s| s.code()).map_or_else(|| "?".to_owned(), |c| c.to_string());
                registrar(pasta, &format!("Rustyfin encerrou com o código {codigo}; reiniciando em {} segundos.", espera.as_secs()));
                if !estado.sair.load(Ordering::SeqCst) {
                    thread::sleep(espera);
                }
                // Se ele durou, o próximo erro volta a esperar pouco.
                espera = if inicio.elapsed() > Duration::from_secs(60) {
                    Duration::from_secs(5)
                } else {
                    (espera * 2).min(Duration::from_secs(60))
                };
            }
        }
        estado.parado.store(true, Ordering::SeqCst);
    }

    // ---------------------------------------------------------------- desligamento gracioso

    /// `POST /admin/api/desligar` na própria máquina: o servidor para as transcodificações, fecha o banco e sai.
    /// Com senha do painel configurada não dá para pedir assim (não guardamos a senha aqui); aí só resta forçar.
    fn pedir_desligamento(porta: u16, configuracao: &HashMap<String, String>) -> bool {
        if configuracao.get("RUSTYFIN_ADMIN_PASSWORD").is_some_and(|s| !s.is_empty()) {
            return false;
        }
        let endereco = SocketAddr::from(([127, 0, 0, 1], porta));
        let Ok(mut conexao) = TcpStream::connect_timeout(&endereco, Duration::from_secs(2)) else {
            return false;
        };
        let _ = conexao.set_read_timeout(Some(Duration::from_secs(3)));
        let pedido = "POST /admin/api/desligar HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        if conexao.write_all(pedido.as_bytes()).is_err() {
            return false;
        }
        let mut resposta = String::new();
        let _ = conexao.read_to_string(&mut resposta);
        resposta.lines().next().is_some_and(|linha| linha.contains(" 202 "))
    }

    /// Pede o desligamento e, se o servidor não sair a tempo, força. Roda numa thread para não travar a bandeja.
    fn desligar_em_segundo_plano(estado: &Arc<Estado>, porta: u16, configuracao: &HashMap<String, String>) {
        let (estado, configuracao) = (estado.clone(), configuracao.clone());
        thread::spawn(move || {
            let pediu = pedir_desligamento(porta, &configuracao);
            let limite = Instant::now() + if pediu { PRAZO_DO_DESLIGAMENTO } else { Duration::ZERO };
            while estado.rodando.load(Ordering::SeqCst) && Instant::now() < limite {
                thread::sleep(Duration::from_millis(200));
            }
            if estado.rodando.load(Ordering::SeqCst) {
                estado.matar.store(true, Ordering::SeqCst);
            }
        });
    }

    // ---------------------------------------------------------------- abrir arquivos e endereços

    /// Abre um endereço, arquivo ou pasta com o programa padrão do Windows.
    fn abrir(destino: &str) {
        let _ = Command::new("cmd")
            .args(["/c", "start", "", destino])
            .creation_flags(SEM_JANELA)
            .spawn();
    }
}
