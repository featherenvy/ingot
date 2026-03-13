export function shortOid(value: string | null) {
  return value ? value.slice(0, 8) : '—'
}

export function shortId(value: string) {
  return value.slice(0, 8)
}
