package orchestrator

import (
	"fmt"
	"strings"
)

// workerPrompt builds the headless brief each worker receives: its task
// wrapped in the ODD workflow contract (explore → plan → implement → check
// → close) with proportional verification, plus the output protocol the
// launcher parses (DECISION: lines and the final SUMMARY line). It is a
// distilled sibling of the repo's canonical odd-orchestrator sections,
// compact enough for every supported headless CLI.
func workerPrompt(s Session) string {
	var b strings.Builder
	fmt.Fprintf(&b, "Task: %s\n", strings.TrimSpace(s.Task))
	fmt.Fprintf(&b, "Branch: %s (you own this branch and its worktree)\n\n", s.Branch)

	b.WriteString(`## Fases (ODD)
1. Explora el área afectada antes de escribir.
2. Planea en una línea qué archivos vas a tocar.
3. Implementa el cambio más pequeño que cumpla la tarea.
4. Verifica proporcional al riesgo: pasivo, lectura estructural; medio, tus tests; alto (seguridad, datos, concurrencia), tests + verificación extra.
5. Cierra dejando todo verde y la rama con commits limpios.

## Verificación
- La normalización (formatters, generadores) va antes de la verificación; después solo chequeos.
- Proporcional al riesgo: pasivo = lectura estructural; medio = tus tests; alto = tests + verificación extra.
- Si una verificación requerida falla por el entorno, repórtalo en el SUMMARY; nunca inventes un pase.

## Protocolo de salida (obligatorio)
- DECISION: <qué elegiste y por qué> — cada decisión relevante, en su propia línea.
- SUMMARY: <qué hiciste, por qué, y el resultado de cada verificación> — una línea al terminar.
`)
	return b.String()
}
