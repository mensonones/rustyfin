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
Source: "firewall.ps1"; DestDir: "{app}"; Flags: ignoreversion
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

function Parametro(const Nome, Padrao: string): string;
begin
  Result := ExpandConstant('{param:' + Nome + '|' + Padrao + '}');
end;

function UrlPainel(Param: string): string;
begin
  Result := 'http://localhost:' + PortaEscolhida + '/admin';
end;

{ ---------------------------------------------------------------- ffmpeg }

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
  if FfmpegParametro <> '' then
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
  Modo, Padrao: string;
  Texto: TNewStaticText;
  BotaoAdicionar, BotaoRemover: TNewButton;
begin
  PortaEscolhida := Parametro('PORTA', '8096');
  FfmpegDetectado := ProcurarFfmpeg;

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
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  Porta: Integer;
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
    Exit;
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

procedure CurStepChanged(CurStep: TSetupStep);
var
  Codigo: Integer;
  Argumentos: string;
begin
  if CurStep = ssInstall then
  begin
    { Atualização: para o servidor antes de trocar os arquivos. }
    if FileExists(ExpandConstant('{app}\tarefa.ps1')) then
      ExecutarPS('tarefa.ps1', ArgumentosTarefa('Parar'));
  end;
  if CurStep = ssPostInstall then
  begin
    EscreverConfiguracao;
    Argumentos := ArgumentosTarefa('Registrar');
    if not WizardIsTaskSelected('inicio') then
      Argumentos := Argumentos + ' -SemGatilho';
    if not WizardIsTaskSelected('bandeja') then
      Argumentos := Argumentos + ' -SemBandeja';
    ExecutarPS('tarefa.ps1', Argumentos);
    ExecutarPS('tarefa.ps1', ArgumentosTarefa('Iniciar'));
    if WizardIsTaskSelected('firewall') then
    begin
      { `runas` mostra o pedido de permissão do Windows; se você recusar, o resto da instalação segue. }
      if ShellExec('runas', 'powershell.exe', '-NoProfile -ExecutionPolicy Bypass -File "'
          + ExpandConstant('{app}\firewall.ps1') + '" -Porta ' + PortaEscolhida, '', SW_HIDE, ewWaitUntilTerminated, Codigo)
          and (Codigo = 0) then
        SaveStringToFile(ExpandConstant('{app}\firewall.flag'), 'regras criadas', False);
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
    if FileExists(ExpandConstant('{app}\firewall.flag')) then
      ShellExec('runas', 'powershell.exe', '-NoProfile -ExecutionPolicy Bypass -File "'
        + ExpandConstant('{app}\firewall.ps1') + '" -Remover', '', SW_HIDE, ewWaitUntilTerminated, Codigo);
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
