# Qual perfil de execução é o melhor nesta máquina

`escolher_perfil.py` mede a transcodificação em cada perfil que a máquina permitir (Docker ou nativo, processador ou
GPU) e recomenda um. Não altera nada no sistema: grava só numa pasta temporária. Usa só a biblioteca padrão.

```bash
python tools/perfil/escolher_perfil.py                                 # Linux, macOS
python tools/perfil/escolher_perfil.py --ffmpeg-dir C:\ffmpeg\bin      # Windows
python tools/perfil/escolher_perfil.py --json --out resultado.json     # JSON para uma LLM ou um script
```

Explicação, resultado de uma máquina real e como aplicar cada perfil: [docs/escolher-perfil.md](../../docs/escolher-perfil.md).
Procedimento para uma LLM medir e responder por você: [docs/guia-llm.md](../../docs/guia-llm.md).
Exemplo de saída: [`exemplos/i7-11800h-uhd-windows.json`](exemplos/i7-11800h-uhd-windows.json).
