/**
 * Formata o horário de conclusão gravado pelo backend.
 *
 * O valor chega como ISO 8601 já no fuso de Brasília, com o offset embutido
 * (`2026-10-01T14:32:05-03:00`). Como o que importa é o horário de relógio em
 * Brasília, lemos só a parte da data e da hora: passar o valor por `new Date()`
 * converteria para o fuso do navegador e mostraria a hora errada para quem não
 * está em Brasília.
 */
export function formatCompletedAt(iso: string | null): string | null {
  if (!iso) return null;

  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(iso);
  if (!match) return null;

  const [, year, month, day, hour, minute] = match;
  return `${day}/${month}/${year} ${hour}:${minute}`;
}
