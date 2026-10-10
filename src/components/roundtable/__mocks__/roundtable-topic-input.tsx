// Test double: the rich @ editor is covered by its own tests; flows that only
// need a topic string drive this plain textarea instead.
export function RoundtableTopicInput({
  value,
  onChange,
  onBlur,
  invalid,
  describedBy,
}: {
  value: string
  onChange: (value: string) => void
  onBlur?: () => void
  invalid?: boolean
  describedBy?: string
}) {
  return (
    <textarea
      aria-label="topic"
      aria-invalid={invalid || undefined}
      aria-describedby={describedBy}
      value={value}
      onBlur={onBlur}
      onChange={(event) => onChange(event.target.value)}
    />
  )
}
