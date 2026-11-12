# Orchestratord — the fleet daemon

`orchestratord` es el daemon de orquestación multi-agente de este repo: una
herramienta hermana de la app GPUI, en Go, que vive junto al código de la app
pero corre como proceso propio (localhost HTTP + SSE + WebSocket).

## Qué hace

- **Flota**: cada tarea en su worktree aislado (guardia de ramas protegidas).
- **Lanes por hechos**: Working / Needs you / In review / Ready to merge,
  derivadas de qué hace el agente, su PR, CI y reviews — nunca estado manual.
- **Routing**: checks fallidos y review comments vuelven a la sesión dueña
  (presupuesto de fixes acotado, luego escala al humano).
- **Recibos RDD**: `rcpt1_` congelado al salir el worker, gate de ready-to-merge.
- **ODD por worker**: cada worker recibe el brief de Outcome-Driven Development
  y emite DECISION/SUMMARY que el launcher captura y publica a Engram.
- **Memoria de flota**: decisiones y outcomes publicados a Engram (degrada
  silencioso si el servidor no está).
- **Ciclo de vida probado**: daemon muere → workers mueren (incluye nietos),
  verificado con pgrep. Sin ventanas de consola en Windows (GUI subsystem +
  CREATE_NO_WINDOW + HideWindow).
- **Trust boundary local**: Host allow-list (rebinding→403), origin allow-list
  para WS, CORS restringido al shell.
- **Persistencia**: `sessions.json` atómico, rehidratación al reiniciar.
- **Detección de agentes**: 12 CLIs escaneados en PATH con versión, ocultos.

## API (localhost)

```
GET    /board                   Lanes agrupadas por proyecto
GET    /board/live              SSE del board (push cada 5s + heartbeat)
GET    /agents                  Agentes CLI detectados en esta máquina
GET    /projects                Repositorios registrados
POST   /projects                Registrar un repo
GET    /version                 Identidad del build (update banner)
POST   /sessions                Registrar o lanzar un worker (launch:true)
GET    /sessions/{id}/events    Journal de la sesión
GET    /sessions/{id}/diff      Diff del worktree contra su base
GET    /sessions/{id}/evidence  Paquete de evidencia + recibo + valid flag
GET    /sessions/{id}/terminal  WebSocket del PTY embebido
POST   /sessions/{id}/chat      Mensaje humano al stdin del worker
POST   /sessions/{id}/stop      Detener el worker y su árbol de procesos
GET/POST /config                orchestrator.json (repo, repoPath, listen)
```

## Build

```bash
cd orchestratord
go build ./cmd/orchestratord

# Windows (GUI, sin ventana de consola):
GOOS=windows GOARCH=amd64 go build -ldflags="-H=windowsgui" ./cmd/orchestratord
```

## Uso

```bash
orchestratord --repo OWNER/REPO --repo-path /ruta/al/checkout [--listen 127.0.0.1:8094] [--poll 30s] [--print-board]
```

Sin `--repo`: sirve el board sin polling de PRs. Con `--repo-path`: habilita
lanzar workers reales desde el panel (crea worktrees y spawnea el CLI).

Configuración opcional en `orchestrator.json` junto al binario:

```json
{ "repo": "owner/repo", "repoPath": "C:\\ruta\\al\\checkout", "listen": "127.0.0.1:8094" }
```

El shell GPUI (`crates/app/src/features/orchestrator/sidecar.rs`) lee ese mismo
archivo para spawn-earlo cuando abrís el panel Fleet.
