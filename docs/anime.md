# Anime

Uma biblioteca do tipo **Anime** funciona como uma biblioteca de séries para os apps do Jellyfin (eles a veem como
`tvshows`), com três diferenças: metadados do AniList, áudio japonês como padrão e a mesma organização de pastas das séries.

## Como organizar as pastas

O Rustyfin lê o nome dos arquivos e das pastas; não precisa de um padrão especial além do das séries:

```text
Animes/
  Sousou no Frieren (2023)/
    Season 01/
      Sousou no Frieren - S01E01 - O fim da jornada.mkv
      Sousou no Frieren - S01E02.mkv
    Specials/
      Sousou no Frieren - S00E01 - OVA.mkv
```

- **Uma pasta por série**, com o ano entre parênteses quando houver mais de uma obra com o mesmo nome. O ano ajuda a
  escolher a obra certa no AniList.
- **`SxxEyy` no nome do arquivo.** Grupos entre colchetes (`[SubsPlease]`) e marcas de release (`1080p`, `x265`) são
  ignorados. Especiais e OVAs vão em `Season 00` ou `Specials` (`S00E01`).
- **O nome da pasta é o nome da série**: ele é o que se procura no AniList, então prefira o título romaji ou o inglês
  (`Sousou no Frieren` ou `Frieren: Beyond Journey's End`).
- Numeração absoluta (`Título - 105.mkv`, sem temporada) **não** é interpretada: renomeie para `S01E105` ou separe por
  temporada. Para pedir isso a um assistente de IA, passe os exemplos acima.
- Filmes de anime ficam na biblioteca de Filmes (ou, se a pasta for só de anime, aparecem na mesma biblioteca).

## Metadados (AniList)

Para cada série de uma biblioteca Anime, o Rustyfin consulta o [AniList](https://anilist.co) (API pública, **sem conta
nem chave**) e preenche **só o que o TMDB não trouxe**: sinopse (em inglês, que é como o AniList a fornece), capa, fundo,
gêneros, nota e estúdio. O que o TMDB já preencheu, como a sinopse em português, não é trocado. Os episódios continuam com
os dados do TMDB, quando há: o AniList não tem dados por episódio.

- A escolha da obra compara o nome da série com os títulos do AniList (romaji, inglês, original e sinônimos) e usa o ano
  como desempate. Sem correspondência segura, a série fica sem dados em vez de receber os de outro anime. Tente de novo
  depois de renomear a pasta; as séries sem dados são consultadas de novo a cada 7 dias.
- A numeração de temporadas dos fansubs nem sempre bate com a do AniList, onde cada temporada costuma ser uma obra
  separada. Por isso os dados são da série (a primeira temporada), e não de cada temporada.
- **Privacidade:** só os nomes das séries das bibliotecas Anime são enviados ao AniList. Para não enviar nada, defina
  `RUSTYFIN_ANILIST=0`.

## Áudio e legenda padrão

Nos títulos de uma biblioteca Anime, para quem **não escolheu** idioma de áudio nem de legenda, o servidor indica ao app o
**áudio em japonês** e, se o áudio que vai tocar não é português, a **legenda em português** (a completa, não a de falas
forçadas). É o modo "Inteligente" com japonês e português. O app pode ignorar a indicação se o usuário escolher outra faixa,
e qualquer escolha do usuário em [idioma do áudio e da legenda](reproducao.md#idioma-do-áudio-e-da-legenda) vale no lugar
desse padrão.
