; Setup do Rustyfin para Windows (Inno Setup 6). Instala só para o seu usuário, sem pedir administrador.
;
; Compilar (a partir da raiz do repositório, com o rustyfin.exe de release já compilado):
;   ISCC.exe /DAppVersion=0.25.0 packaging\windows\rustyfin.iss          -> dist\Rustyfin-Setup-0.25.0-x64.exe
;   (opcional) /DExePath=caminho\do\rustyfin.exe
;
; Instalação sem telas (para testes e para quem administra vários PCs). Todos os parâmetros são opcionais:
;   Rustyfin-Setup.exe /VERYSILENT /SUPPRESSMSGBOXES /DIR="C:\Rustyfin" /PORTA=8097 /MIDIAS="D:\Filmes;D:\Series"
;                      /FFMPEG=detectar|baixar|nenhum|C:\caminho\ffmpeg.exe /TASKS="inicio,firewall"
; Desinstalar sem telas:  unins000.exe /VERYSILENT /APAGARDADOS=1     (sem o parâmetro, os dados ficam)

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef ExePath
  #define ExePath "..\..\target\release\rustyfin.exe"
#endif
#ifndef TrayPath
  ; O ícone da bandeja fica ao lado do servidor: rustyfin-tray.exe.
  #define TrayPath ExtractFilePath(ExePath) + "rustyfin-tray.exe"
#endif

[Setup]
AppId={{6F1D2B7A-3C4E-4A55-9E0B-8D52C1A7B3F4}
AppName=Rustyfin
AppVersion={#AppVersion}
AppPublisher=Rustyfin
AppPublisherURL=https://github.com/mensonones/rustyfin
AppSupportURL=https://github.com/mensonones/rustyfin/issues
AppUpdatesURL=https://github.com/mensonones/rustyfin/releases
AppComments=Servidor de mídia compatível com os apps do Jellyfin
VersionInfoVersion={#AppVersion}
DefaultDirName={localappdata}\Programs\Rustyfin
DisableProgramGroupPage=yes
DefaultGroupName=Rustyfin
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir=..\..\dist
OutputBaseFilename=Rustyfin-Setup-{#AppVersion}-x64
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
LicenseFile=..\..\LICENSE
UninstallDisplayName=Rustyfin
UninstallDisplayIcon={app}\rustyfin.exe
SetupIconFile=rustyfin.ico
CloseApplications=no
RestartApplications=no

[Languages]
Name: "brazilianportuguese"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"

[Tasks]
Name: "inicio"; Description: "Iniciar o Rustyfin quando eu entrar no Windows"
Name: "bandeja"; Description: "Mostrar o ícone do Rustyfin na bandeja do sistema, com o menu para abrir o painel e sair"
Name: "firewall"; Description: "Liberar o Firewall na rede privada, para o celular e a TV encontrarem o servidor (pede permissão de administrador)"; Flags: unchecked

[Files]
Source: "{#ExePath}"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#TrayPath}"; DestDir: "{app}"; Flags: ignoreversion
Source: "iniciar.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "tarefa.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "porta.ps1"; Flags: dontcopy
Source: "..\rustyfin.env.example"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Rustyfin\Rustyfin (bandeja)"; Filename: "{app}\rustyfin-tray.exe"; WorkingDir: "{app}"
Name: "{autoprograms}\Rustyfin\Painel do Rustyfin"; Filename: "{cmd}"; Parameters: "/c start """" ""{code:UrlPainel}"""; Flags: runminimized
Name: "{autoprograms}\Rustyfin\Desinstalar o Rustyfin"; Filename: "{uninstallexe}"

[Run]
Filename: "{code:UrlPainel}"; Description: "Abrir o painel de administração"; Flags: shellexec postinstall skipifsilent nowait

[UninstallDelete]
Type: filesandordirs; Name: "{app}\ffmpeg"

[Code]
var
  PaginaFfmpeg: TInputOptionWizardPage;
  PaginaMidias: TWizardPage;
  ListaMidias: TNewListBox;
  PaginaRede: TInputQueryWizardPage;
  PaginaDownload: TDownloadWizardPage;
  FfmpegDetectado: string;
  FfmpegParametro: string;
  PortaEscolhida: string;
  PaginaAtualizacao: TOutputMsgWizardPage;
  { Atualização: há uma instalação anterior (achada pelo registro de desinstalação, antes de a pasta ser escolhida). }
  Atualizacao: Boolean;
  PastaInstalada: string;
  VersaoInstalada: string;
  FfmpegPrecisaReparo: Boolean;

const
  ChaveDesinstalar = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{6F1D2B7A-3C4E-4A55-9E0B-8D52C1A7B3F4}_is1';

function Parametro(const Nome, Padrao: string): string;
begin
  Result := ExpandConstant('{param:' + Nome + '|' + Padrao + '}');
end;

function UrlPainel(Param: string): string;
begin
  Result := 'http://localhost:' + PortaEscolhida + '/admin';
end;

{ ---------------------------------------------------------------- ffmpeg }
function EmAtualizacao: Boolean;
begin
  Result := Atualizacao and FileExists(ExpandConstant('{app}\rustyfin.env'));
end;


function ProcurarFfmpeg: string;
var
  Locais: string;
begin
  Locais := GetEnv('PATH') + ';C:\ffmpeg\bin;' + ExpandConstant('{localappdata}\Microsoft\WinGet\Links')
    + ';' + ExpandConstant('{commonpf}\ffmpeg\bin');
  Result := FileSearch('ffmpeg.exe', Locais);
  { O servidor também precisa do ffprobe, ao lado do ffmpeg. }
  if (Result <> '') and (not FileExists(ExtractFilePath(Result) + 'ffprobe.exe')) then
    Result := '';
end;

function TemQuickSync(const Ffmpeg: string): Boolean;
var
  Saida: string;
  Conteudo: AnsiString;
  Codigo: Integer;
begin
  Result := False;
  Saida := ExpandConstant('{tmp}\ffmpeg-encoders.txt');
  if Exec(ExpandConstant('{cmd}'), '/c ""' + Ffmpeg + '" -hide_banner -encoders > "' + Saida + '" 2>&1"',
      '', SW_HIDE, ewWaitUntilTerminated, Codigo) then
    if LoadStringFromFile(Saida, Conteudo) then
      Result := Pos('h264_qsv', String(Conteudo)) > 0;
end;

function ModoFfmpeg: string;
begin
  if EmAtualizacao and (not FfmpegPrecisaReparo) then
    Result := 'manter'
  else if FfmpegParametro <> '' then
    Result := 'caminho'
  else if PaginaFfmpeg.Values[1] then
    Result := 'baixar'
  else if PaginaFfmpeg.Values[2] then
    Result := 'nenhum'
  else
    Result := 'usar';
end;

{ Escolhe, na lista de checksums do BtbN, o build estável mais novo (ffmpeg-nX.Y-latest-win64-gpl-X.Y.zip) e, se não
  houver, o `master`. Devolve o nome do arquivo e o SHA-256 que o download precisa ter. }
function EscolherArquivoFfmpeg(var Nome, Hash: string): Boolean;
var
  Linhas: TArrayOfString;
  I, P, Maior, Versao, Ponto: Integer;
  Linha, Candidato, Ver: string;
begin
  Result := False;
  Maior := -1;
  if not LoadStringsFromFile(ExpandConstant('{tmp}\checksums.sha256'), Linhas) then
    Exit;
  for I := 0 to GetArrayLength(Linhas) - 1 do
  begin
    Linha := Trim(Linhas[I]);
    P := Pos(' ', Linha);
    if P < 65 then
      Continue;
    Candidato := Trim(Copy(Linha, P + 1, 255));
    if (Pos('ffmpeg-n', Candidato) = 1) and (Pos('-latest-win64-gpl-', Candidato) > 0)
        and (Pos('shared', Candidato) = 0) and (Pos('.zip', Candidato) > 0) then
    begin
      Ver := Copy(Candidato, 9, Pos('-latest', Candidato) - 9);
      Ponto := Pos('.', Ver);
      if Ponto > 0 then
        Versao := StrToIntDef(Copy(Ver, 1, Ponto - 1), 0) * 1000 + StrToIntDef(Copy(Ver, Ponto + 1, 10), 0)
      else
        Versao := StrToIntDef(Ver, 0) * 1000;
      if Versao > Maior then
      begin
        Maior := Versao;
        Nome := Candidato;
        Hash := Lowercase(Copy(Linha, 1, 64));
      end;
    end;
  end;
  if Maior < 0 then
    for I := 0 to GetArrayLength(Linhas) - 1 do
    begin
      Linha := Trim(Linhas[I]);
      P := Pos(' ', Linha);
      if (P >= 65) and (Trim(Copy(Linha, P + 1, 255)) = 'ffmpeg-master-latest-win64-gpl.zip') then
      begin
        Nome := 'ffmpeg-master-latest-win64-gpl.zip';
        Hash := Lowercase(Copy(Linha, 1, 64));
        Maior := 0;
      end;
    end;
  Result := Maior >= 0;
end;

function OnDownloadProgress(const Url, FileName: string; const Progress, ProgressMax: Int64): Boolean;
begin
  Result := True;
end;

{ ---------------------------------------------------------------- atualização }

{ "0.25.5" vira um número comparável (até três partes). }
function NumeroDaVersao(const Versao: string): Integer;
var
  Resto, Parte: string;
  I, P: Integer;
begin
  Result := 0;
  Resto := Versao;
  for I := 1 to 3 do
  begin
    P := Pos('.', Resto);
    if P > 0 then
    begin
      Parte := Copy(Resto, 1, P - 1);
      Resto := Copy(Resto, P + 1, 100);
    end
    else
    begin
      Parte := Resto;
      Resto := '';
    end;
    Result := Result * 1000 + StrToIntDef(Parte, 0);
  end;
end;

{ O valor de uma chave ativa (não comentada) do rustyfin.env; vazio se não houver. }
function ValorDoEnv(const Arquivo, Chave: string): string;
var
  Linhas: TArrayOfString;
  I: Integer;
  Linha: string;
begin
  Result := '';
  if not LoadStringsFromFile(Arquivo, Linhas) then
    Exit;
  for I := 0 to GetArrayLength(Linhas) - 1 do
  begin
    Linha := Trim(Linhas[I]);
    if Pos(Chave + '=', Linha) = 1 then
    begin
      Result := Trim(Copy(Linha, Length(Chave) + 2, 1000));
      if (Length(Result) >= 2) and (Result[1] = '"') and (Result[Length(Result)] = '"') then
        Result := Copy(Result, 2, Length(Result) - 2);
    end;
  end;
end;

{ O ffmpeg.exe configurado existe (e tem o ffprobe ao lado)? Um nome sem pasta é procurado no PATH. }
function FfmpegConfiguradoFunciona(const Configurado: string): Boolean;
var
  Caminho: string;
begin
  Caminho := Configurado;
  if (Pos('\', Caminho) = 0) and (Pos(':', Caminho) = 0) then
    Caminho := FileSearch(Caminho, GetEnv('PATH'));
  Result := (Caminho <> '') and FileExists(Caminho) and FileExists(ExtractFilePath(Caminho) + 'ffprobe.exe');
end;

procedure DetectarInstalacaoAnterior;
var
  Pasta, Versao: string;
begin
  Atualizacao := False;
  if not RegQueryStringValue(HKCU, ChaveDesinstalar, 'Inno Setup: App Path', Pasta) then
    Exit;
  Pasta := RemoveBackslashUnlessRoot(Pasta);
  if not FileExists(Pasta + '\rustyfin.env') then
    Exit;
  Atualizacao := True;
  PastaInstalada := Pasta;
  if not RegQueryStringValue(HKCU, ChaveDesinstalar, 'DisplayVersion', Versao) then
    Versao := '';
  VersaoInstalada := Versao;
end;

{ Troca a linha do ffmpeg no rustyfin.env existente; o resto da configuração fica como está. }
procedure AtualizarFfmpegNoEnv(const Arquivo, Novo: string);
var
  Linhas, Saida: TArrayOfString;
  I, N: Integer;
  Linha: string;
  Havia: Boolean;
begin
  if not LoadStringsFromFile(Arquivo, Linhas) then
    Exit;
  N := 0;
  Havia := False;
  SetArrayLength(Saida, GetArrayLength(Linhas) + 1);
  for I := 0 to GetArrayLength(Linhas) - 1 do
  begin
    Linha := Trim(Linhas[I]);
    if Pos('RUSTYFIN_FFMPEG=', Linha) <> 1 then
    begin
      Saida[N] := Linhas[I];
      N := N + 1;
    end
    else
      Havia := True;
  end;
  { Sem ffmpeg configurado e nada a trocar: o arquivo já está como deve ficar. }
  if (Novo = '') and (not Havia) then
    Exit;
  if Novo <> '' then
    Saida[N] := 'RUSTYFIN_FFMPEG=' + Novo
  else
    Saida[N] := '# RUSTYFIN_FFMPEG=C:\caminho\ffmpeg.exe   (sem o ffmpeg só há reprodução direta)';
  SetArrayLength(Saida, N + 1);
  SaveStringsToUTF8File(Arquivo, Saida, False);
end;

{ ---------------------------------------------------------------- páginas do assistente }

procedure AdicionarPastaClick(Sender: TObject);
var
  Pasta: string;
begin
  Pasta := '';
  if BrowseForFolder('Escolha uma pasta com filmes ou séries', Pasta, False) then
    if ListaMidias.Items.IndexOf(Pasta) < 0 then
      ListaMidias.Items.Add(Pasta);
end;

procedure RemoverPastaClick(Sender: TObject);
begin
  if ListaMidias.ItemIndex >= 0 then
    ListaMidias.Items.Delete(ListaMidias.ItemIndex);
end;

procedure InitializeWizard;
var
  Modo, Padrao, Configurado, Porta, Situacao, Titulo: string;
  Texto: TNewStaticText;
  BotaoAdicionar, BotaoRemover: TNewButton;
begin
  PortaEscolhida := Parametro('PORTA', '8096');
  FfmpegDetectado := ProcurarFfmpeg;
  DetectarInstalacaoAnterior;

  PaginaFfmpeg := CreateInputOptionPage(wpSelectDir, 'FFmpeg', 'Transcodificação, miniaturas e legendas',
    'O Rustyfin usa o ffmpeg para converter os vídeos que a TV ou o celular não tocam. Sem ele só há reprodução direta '
    + '(Direct Play). Como você quer configurá-lo?', True, False);
  PaginaFfmpeg.Add('Usar o ffmpeg já instalado');
  PaginaFfmpeg.Add('Baixar e instalar o ffmpeg (cerca de 200 MB, build oficial com Quick Sync, verificado por SHA-256)');
  PaginaFfmpeg.Add('Configurar depois (só reprodução direta)');
  if FfmpegDetectado <> '' then
  begin
    if TemQuickSync(FfmpegDetectado) then
      PaginaFfmpeg.CheckListBox.ItemCaption[0] := 'Usar o ffmpeg encontrado em ' + FfmpegDetectado + ' (com Quick Sync)'
    else
      PaginaFfmpeg.CheckListBox.ItemCaption[0] := 'Usar o ffmpeg encontrado em ' + FfmpegDetectado + ' (sem Quick Sync)';
    PaginaFfmpeg.Values[0] := True;
  end
  else
  begin
    PaginaFfmpeg.CheckListBox.ItemCaption[0] := 'Usar o ffmpeg já instalado (nenhum foi encontrado)';
    PaginaFfmpeg.CheckListBox.ItemEnabled[0] := False;
    PaginaFfmpeg.Values[1] := True;
  end;

  { Instalação sem telas: /FFMPEG=detectar|baixar|nenhum|<caminho do ffmpeg.exe> }
  Modo := Lowercase(Parametro('FFMPEG', 'detectar'));
  if Modo = 'baixar' then
    PaginaFfmpeg.Values[1] := True
  else if Modo = 'nenhum' then
    PaginaFfmpeg.Values[2] := True
  else if (Modo <> 'detectar') and FileExists(Parametro('FFMPEG', '')) then
    FfmpegParametro := Parametro('FFMPEG', '');

  PaginaMidias := CreateCustomPage(PaginaFfmpeg.ID, 'Pastas de mídia', 'Onde estão os seus filmes e séries?');
  Texto := TNewStaticText.Create(PaginaMidias);
  Texto.Parent := PaginaMidias.Surface;
  Texto.WordWrap := True;
  Texto.AutoSize := True;
  Texto.Width := PaginaMidias.SurfaceWidth;
  Texto.Caption := 'Adicione as pastas com os seus vídeos. As bibliotecas "Filmes" e "Séries" são criadas com elas na '
    + 'primeira execução, e depois você muda tudo no painel. Pode deixar a lista vazia e configurar depois.';
  ListaMidias := TNewListBox.Create(PaginaMidias);
  ListaMidias.Parent := PaginaMidias.Surface;
  ListaMidias.Top := Texto.Height + ScaleY(12);
  ListaMidias.Left := 0;
  ListaMidias.Width := PaginaMidias.SurfaceWidth - ScaleX(118);
  ListaMidias.Height := PaginaMidias.SurfaceHeight - ListaMidias.Top;
  BotaoAdicionar := TNewButton.Create(PaginaMidias);
  BotaoAdicionar.Parent := PaginaMidias.Surface;
  BotaoAdicionar.Caption := 'Adicionar pasta...';
  BotaoAdicionar.Left := ListaMidias.Width + ScaleX(8);
  BotaoAdicionar.Top := ListaMidias.Top;
  BotaoAdicionar.Width := ScaleX(110);
  BotaoAdicionar.Height := ScaleY(25);
  BotaoAdicionar.OnClick := @AdicionarPastaClick;
  BotaoRemover := TNewButton.Create(PaginaMidias);
  BotaoRemover.Parent := PaginaMidias.Surface;
  BotaoRemover.Caption := 'Remover';
  BotaoRemover.Left := BotaoAdicionar.Left;
  BotaoRemover.Top := BotaoAdicionar.Top + ScaleY(31);
  BotaoRemover.Width := ScaleX(110);
  BotaoRemover.Height := ScaleY(25);
  BotaoRemover.OnClick := @RemoverPastaClick;
  Padrao := ExpandConstant('{%USERPROFILE}\Videos');
  if DirExists(Padrao) then
    ListaMidias.Items.Add(Padrao);

  PaginaRede := CreateInputQueryPage(PaginaMidias.ID, 'Rede', 'Porta do servidor',
    'Os apps se conectam por esta porta. Mude só se a 8096 estiver ocupada, por exemplo por outro servidor de mídia.');
  PaginaRede.Add('Porta (de 1024 a 65535):', False);
  PaginaRede.Values[0] := PortaEscolhida;

  PaginaDownload := CreateDownloadPage(SetupMessage(msgWizardPreparing), SetupMessage(msgPreparingDesc),
    @OnDownloadProgress);

  { Atualização ou reinstalação: confere o que já está configurado e só pergunta o que precisa de atenção. }
  PaginaAtualizacao := CreateOutputMsgPage(wpWelcome, 'Atualização', 'O Rustyfin já está instalado', '');
  if Atualizacao then
  begin
    Configurado := ValorDoEnv(PastaInstalada + '\rustyfin.env', 'RUSTYFIN_FFMPEG');
    Porta := ValorDoEnv(PastaInstalada + '\rustyfin.env', 'RUSTYFIN_PORT');
    if StrToIntDef(Porta, 0) > 0 then
      PortaEscolhida := Porta
    else
      Porta := '8096';
    if Configurado = '' then
      Situacao := 'ffmpeg: não configurado (só reprodução direta). Na próxima tela você pode instalá-lo.'
    else if FfmpegConfiguradoFunciona(Configurado) then
      Situacao := 'ffmpeg: ok (' + Configurado + ')'
    else
      Situacao := 'ffmpeg: NÃO encontrado em ' + Configurado + ' (ou falta o ffprobe ao lado). Na próxima tela você '
        + 'escolhe como corrigir.';
    FfmpegPrecisaReparo := (Configurado = '') or (not FfmpegConfiguradoFunciona(Configurado));
    { Sem telas, só mexe no ffmpeg se isso foi pedido com /FFMPEG=. }
    if WizardSilent and (Parametro('FFMPEG', '') = '') then
      FfmpegPrecisaReparo := False;

    if VersaoInstalada = '' then
      Titulo := 'Foi encontrada uma instalação do Rustyfin em ' + PastaInstalada + '.'
    else if NumeroDaVersao(VersaoInstalada) = NumeroDaVersao('{#AppVersion}') then
      Titulo := 'O Rustyfin ' + VersaoInstalada + ' já está instalado. Este assistente vai reparar a instalação.'
    else if NumeroDaVersao(VersaoInstalada) > NumeroDaVersao('{#AppVersion}') then
      Titulo := 'Atenção: está instalado o Rustyfin ' + VersaoInstalada + ', mais novo que esta versão ({#AppVersion}). '
        + 'Continuar volta para a versão anterior.'
    else
      Titulo := 'Atualizando o Rustyfin ' + VersaoInstalada + ' para a versão {#AppVersion}.';
    PaginaAtualizacao.MsgLabel.Caption := Titulo + #13#10#13#10 + 'Mantidos: a configuração (rustyfin.env), o banco de dados, '
      + 'as bibliotecas, os usuários e o log. O servidor é parado e iniciado de novo pelo assistente.' + #13#10#13#10
      + 'Porta: ' + Porta + ' (mantida)' + #13#10 + Situacao;
  end;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := False;
  if PageID = PaginaAtualizacao.ID then
    Result := not Atualizacao
  else if (PageID = PaginaMidias.ID) or (PageID = PaginaRede.ID) then
    Result := EmAtualizacao
  else if PageID = PaginaFfmpeg.ID then
    Result := EmAtualizacao and (not FfmpegPrecisaReparo);
end;

{ Confere se uma porta TCP está livre (porta.ps1, extraído para a pasta temporária). Se não der para conferir, não atrapalha. }
function PortaLivre(Porta: Integer): Boolean;
var
  Codigo: Integer;
begin
  Result := True;
  ExtractTemporaryFile('porta.ps1');
  if Exec('powershell.exe', '-NoProfile -ExecutionPolicy Bypass -File "' + ExpandConstant('{tmp}\porta.ps1') + '" -Porta '
      + IntToStr(Porta), '', SW_HIDE, ewWaitUntilTerminated, Codigo) then
    Result := (Codigo = 0);
end;

{ A primeira porta livre a partir da informada (a própria informada, se der). }
function ProximaPortaLivre(Porta: Integer): Integer;
var
  Arquivo: string;
  Conteudo: AnsiString;
  Codigo: Integer;
begin
  Result := Porta;
  Arquivo := ExpandConstant('{tmp}\porta-livre.txt');
  DeleteFile(Arquivo);
  ExtractTemporaryFile('porta.ps1');
  if Exec('powershell.exe', '-NoProfile -ExecutionPolicy Bypass -File "' + ExpandConstant('{tmp}\porta.ps1') + '" -Porta '
      + IntToStr(Porta) + ' -Procurar -Saida "' + Arquivo + '"', '', SW_HIDE, ewWaitUntilTerminated, Codigo) then
    if (Codigo = 0) and LoadStringFromFile(Arquivo, Conteudo) then
      Result := StrToIntDef(Trim(String(Conteudo)), Porta);
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  Porta, Sugerida, Resposta: Integer;
begin
  Result := True;
  if CurPageID = PaginaRede.ID then
  begin
    Porta := StrToIntDef(Trim(PaginaRede.Values[0]), 0);
    if (Porta < 1024) or (Porta > 65535) then
    begin
      MsgBox('Informe uma porta entre 1024 e 65535.', mbError, MB_OK);
      Result := False;
      Exit;
    end;
    { Porta ocupada (outro servidor de mídia, o Rustyfin no Docker...): avisa e sugere a próxima livre. Numa atualização
      a porta pode ser do próprio Rustyfin, então não pergunta; no modo sem telas também não. }
    if (not WizardSilent) and (not FileExists(ExpandConstant('{app}\rustyfin.exe'))) and (not PortaLivre(Porta)) then
    begin
      Sugerida := ProximaPortaLivre(Porta + 1);
      Resposta := MsgBox('A porta ' + IntToStr(Porta) + ' já está em uso neste computador, por exemplo por outro servidor '
        + 'de mídia ou pelo Rustyfin no Docker.' + #13#10#13#10 + 'Usar a porta ' + IntToStr(Sugerida) + ' no lugar?'
        + #13#10#13#10 + 'Sim: usar a ' + IntToStr(Sugerida) + '.' + #13#10 + 'Não: manter a ' + IntToStr(Porta)
        + ' (pare o outro serviço antes de iniciar o Rustyfin).' + #13#10 + 'Cancelar: voltar e escolher outra.',
        mbConfirmation, MB_YESNOCANCEL);
      if Resposta = IDYES then
      begin
        PaginaRede.Values[0] := IntToStr(Sugerida);
        Porta := Sugerida;
      end
      else if Resposta = IDCANCEL then
      begin
        Result := False;
        Exit;
      end;
    end;
    PortaEscolhida := IntToStr(Porta);
  end;
end;

{ ---------------------------------------------------------------- instalação }

function ExecutarPS(const Script, Argumentos: string): Integer;
var
  Codigo: Integer;
begin
  if not Exec('powershell.exe', '-NoProfile -ExecutionPolicy Bypass -File "' + ExpandConstant('{app}\') + Script + '" '
      + Argumentos, '', SW_HIDE, ewWaitUntilTerminated, Codigo) then
    Codigo := -1;
  Result := Codigo;
end;

function ArgumentosTarefa(const Acao: string): string;
begin
  Result := '-Pasta "' + ExpandConstant('{app}') + '" -Acao ' + Acao;
end;

// Baixa (com a verificação do SHA-256) e extrai o ffmpeg para a pasta ffmpeg do aplicativo, antes de copiar os arquivos.
function PrepareToInstall(var NeedsRestart: Boolean): string;
var
  Nome, Hash, Destino: string;
  Codigo: Integer;
  Baixou: Boolean;
begin
  Result := '';
  if ModoFfmpeg <> 'baixar' then
    Exit;

  Baixou := False;
  PaginaDownload.Clear;
  PaginaDownload.Add('https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/checksums.sha256',
    'checksums.sha256', '');
  PaginaDownload.Show;
  try
    try
      PaginaDownload.Download;
      if EscolherArquivoFfmpeg(Nome, Hash) then
      begin
        PaginaDownload.Clear;
        PaginaDownload.Add('https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/' + Nome, 'ffmpeg.zip', Hash);
        PaginaDownload.Download;
        Baixou := True;
      end
      else
        Result := 'Não foi possível identificar o build do ffmpeg na lista de checksums.';
    except
      if PaginaDownload.AbortedByUser then
        Result := 'O download do ffmpeg foi cancelado.'
      else
        Result := 'Falha ao baixar o ffmpeg: ' + GetExceptionMessage + #13#10#13#10
          + 'Volte e escolha "Configurar depois": dá para instalar o ffmpeg depois (veja a documentação).';
    end;
  finally
    PaginaDownload.Hide;
  end;
  if not Baixou then
    Exit;

  Destino := ExpandConstant('{app}\ffmpeg');
  ForceDirectories(Destino);
  if (not Exec(ExpandConstant('{sys}\tar.exe'), '-xf "' + ExpandConstant('{tmp}\ffmpeg.zip') + '" -C "' + Destino
      + '" --strip-components=1', '', SW_HIDE, ewWaitUntilTerminated, Codigo)) or (Codigo <> 0) then
    Result := 'Não foi possível extrair o ffmpeg (tar.exe, código ' + IntToStr(Codigo) + ').'
  else if not FileExists(Destino + '\bin\ffmpeg.exe') then
    Result := 'O arquivo baixado não tem o ffmpeg.exe esperado.';
end;

procedure AdicionarLinha(var Linhas: TArrayOfString; const Texto: string);
var
  N: Integer;
begin
  N := GetArrayLength(Linhas);
  SetArrayLength(Linhas, N + 1);
  Linhas[N] := Texto;
end;

function ListaDeMidias: string;
var
  I: Integer;
  Pasta: string;
begin
  Result := Parametro('MIDIAS', '');
  StringChangeEx(Result, ';', ',', True);
  if Result <> '' then
    Exit;
  for I := 0 to ListaMidias.Items.Count - 1 do
  begin
    Pasta := Trim(ListaMidias.Items[I]);
    if Pasta <> '' then
    begin
      if Result <> '' then
        Result := Result + ',';
      Result := Result + Pasta;
    end;
  end;
end;

{ Cria o rustyfin.env na primeira instalação. Numa atualização, a configuração existente é preservada. }
procedure EscreverConfiguracao;
var
  Arquivo, Midias, Ffmpeg: string;
  Linhas: TArrayOfString;
begin
  Arquivo := ExpandConstant('{app}\rustyfin.env');
  if FileExists(Arquivo) then
  begin
    { Atualização: o resto da configuração fica como está; só o ffmpeg é corrigido, se foi escolhido. }
    if Atualizacao and FfmpegPrecisaReparo then
    begin
      if ModoFfmpeg = 'baixar' then
        AtualizarFfmpegNoEnv(Arquivo, ExpandConstant('{app}\ffmpeg\bin\ffmpeg.exe'))
      else if ModoFfmpeg = 'caminho' then
        AtualizarFfmpegNoEnv(Arquivo, FfmpegParametro)
      else if ModoFfmpeg = 'usar' then
        AtualizarFfmpegNoEnv(Arquivo, FfmpegDetectado)
      else
        AtualizarFfmpegNoEnv(Arquivo, '');
    end;
    Exit;
  end;
  Midias := ListaDeMidias;
  if ModoFfmpeg = 'baixar' then
    Ffmpeg := ExpandConstant('{app}\ffmpeg\bin\ffmpeg.exe')
  else if ModoFfmpeg = 'caminho' then
    Ffmpeg := FfmpegParametro
  else if ModoFfmpeg = 'usar' then
    Ffmpeg := FfmpegDetectado
  else
    Ffmpeg := '';

  AdicionarLinha(Linhas, '# Configuração do Rustyfin (criada pelo setup). Formato CHAVE=VALOR; reinicie o Rustyfin depois de editar.');
  AdicionarLinha(Linhas, '# Todas as opções: https://github.com/mensonones/rustyfin/blob/main/docs/configuracao.md');
  AdicionarLinha(Linhas, 'RUSTYFIN_PORT=' + PortaEscolhida);
  if Midias <> '' then
  begin
    AdicionarLinha(Linhas, '# Pastas de mídia: só valem na primeira execução; depois use o painel (/admin).');
    AdicionarLinha(Linhas, 'RUSTYFIN_MEDIA_DIRS=' + Midias);
  end;
  if Ffmpeg <> '' then
    AdicionarLinha(Linhas, 'RUSTYFIN_FFMPEG=' + Ffmpeg)
  else
    AdicionarLinha(Linhas, '# RUSTYFIN_FFMPEG=C:\caminho\ffmpeg.exe   (sem o ffmpeg só há reprodução direta)');
  SaveStringsToUTF8File(Arquivo, Linhas, False);
end;

{ A porta que o servidor de fato usa: a do rustyfin.env já existente (numa atualização ela é preservada) ou, na primeira
  instalação, a escolhida no assistente. O firewall e o link do painel precisam seguir a porta efetiva. }
procedure AtualizarPortaEfetiva;
var
  Linhas: TArrayOfString;
  I, Valor: Integer;
  Linha: string;
begin
  if not LoadStringsFromFile(ExpandConstant('{app}\rustyfin.env'), Linhas) then
    Exit;
  for I := 0 to GetArrayLength(Linhas) - 1 do
  begin
    Linha := Trim(Linhas[I]);
    StringChangeEx(Linha, ' ', '', True);
    if Pos('RUSTYFIN_PORT=', Linha) = 1 then
    begin
      Valor := StrToIntDef(Copy(Linha, Length('RUSTYFIN_PORT=') + 1, 10), 0);
      if (Valor >= 1) and (Valor <= 65535) then
        PortaEscolhida := IntToStr(Valor);
    end;
  end;
end;

procedure AvisarFalha(const Texto: string);
begin
  Log(Texto);
  SuppressibleMsgBox(Texto, mbError, MB_OK, IDOK);
end;

{ Regras do Firewall (rede privada) numa única elevação. Só roda programas do próprio Windows (cmd.exe e netsh.exe, em
  System32) com argumentos que o setup monta: nada que o usuário possa alterar é executado como administrador. A porta
  vem sempre de um número validado. }
function ComandoFirewall(Adicionar: Boolean; const Porta: string): string;
var
  Netsh: string;
begin
  Netsh := '"' + ExpandConstant('{sys}\netsh.exe') + '" advfirewall firewall ';
  Result := Netsh + 'delete rule name="Rustyfin (HTTP)" & ' + Netsh + 'delete rule name="Rustyfin (descoberta)"';
  if Adicionar then
    Result := Result + ' & ' + Netsh + 'add rule name="Rustyfin (HTTP)" dir=in action=allow protocol=TCP localport='
      + Porta + ' profile=private & ' + Netsh
      + 'add rule name="Rustyfin (descoberta)" dir=in action=allow protocol=UDP localport=7359 profile=private';
end;

{ `runas` mostra o pedido de permissão do Windows. Devolve False se você recusar ou se o comando falhar. }
function AplicarFirewall(Adicionar: Boolean; const Porta: string): Boolean;
var
  Codigo: Integer;
begin
  Result := ShellExec('runas', ExpandConstant('{sys}\cmd.exe'), '/s /c "' + ComandoFirewall(Adicionar, Porta) + '"', '',
    SW_HIDE, ewWaitUntilTerminated, Codigo) and (Codigo = 0);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Codigo: Integer;
  Argumentos: string;
begin
  if CurStep = ssInstall then
  begin
    { Atualização: para o servidor antes de trocar os arquivos e segue a porta que já estava configurada. }
    AtualizarPortaEfetiva;
    if FileExists(ExpandConstant('{app}\tarefa.ps1')) then
      ExecutarPS('tarefa.ps1', ArgumentosTarefa('Parar'));
  end;
  if CurStep = ssPostInstall then
  begin
    EscreverConfiguracao;
    AtualizarPortaEfetiva;
    Argumentos := ArgumentosTarefa('Registrar');
    if not WizardIsTaskSelected('inicio') then
      Argumentos := Argumentos + ' -SemGatilho';
    if not WizardIsTaskSelected('bandeja') then
      Argumentos := Argumentos + ' -SemBandeja';
    Codigo := ExecutarPS('tarefa.ps1', Argumentos);
    if Codigo <> 0 then
      AvisarFalha('Não foi possível registrar a tarefa agendada do Rustyfin (código ' + IntToStr(Codigo) + '). O servidor '
        + 'não vai subir sozinho. Abra o Rustyfin pelo menu Iniciar, ou rode o setup de novo.')
    else
    begin
      Codigo := ExecutarPS('tarefa.ps1', ArgumentosTarefa('Iniciar'));
      if Codigo <> 0 then
        AvisarFalha('A tarefa do Rustyfin foi criada, mas não foi possível iniciá-la agora (código ' + IntToStr(Codigo)
          + '). Abra o Rustyfin pelo menu Iniciar ou saia e entre no Windows de novo.');
    end;
    if WizardIsTaskSelected('firewall') then
    begin
      if AplicarFirewall(True, PortaEscolhida) then
        SaveStringToFile(ExpandConstant('{app}\firewall.flag'), 'regras criadas', False)
      else
        AvisarFalha('As regras do Firewall não foram criadas (a permissão foi negada ou o comando falhou). O celular e a TV '
          + 'podem não encontrar o servidor: rode o setup de novo e aceite o pedido de permissão.');
    end;
  end;
end;

{ ---------------------------------------------------------------- desinstalação }

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Codigo: Integer;
  Apagar: Boolean;
begin
  if CurUninstallStep = usUninstall then
  begin
    Exec('powershell.exe', '-NoProfile -ExecutionPolicy Bypass -File "' + ExpandConstant('{app}\tarefa.ps1') + '" '
      + ArgumentosTarefa('Remover'), '', SW_HIDE, ewWaitUntilTerminated, Codigo);
    { Remove as regras do Firewall (um pedido de permissão), também só com programas do Windows. }
    if FileExists(ExpandConstant('{app}\firewall.flag')) then
      AplicarFirewall(False, '0');
  end;
  if CurUninstallStep = usPostUninstall then
  begin
    if UninstallSilent then
      Apagar := ExpandConstant('{param:APAGARDADOS|0}') = '1'
    else
      Apagar := MsgBox('Apagar também a configuração, o banco de dados e o log do Rustyfin?', mbConfirmation,
        MB_YESNO or MB_DEFBUTTON2) = IDYES;
    if Apagar then
      DelTree(ExpandConstant('{app}'), True, True, True);
  end;
end;
