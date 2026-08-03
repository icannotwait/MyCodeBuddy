"use client"

interface DropdownRadioItemContentProps {
  label: string
  description?: string | null
}

export function DropdownRadioItemContent({
  label,
  description,
}: DropdownRadioItemContentProps) {
  const normalizedDescription = description?.trim()

  return (
    <div className="w-full min-w-0 pr-2" title={label}>
      {/* break-all: Cursor compound model ids are long single tokens
          (`…[…,effort=high]`); truncate would hide the distinguishing suffix. */}
      <p className="break-all whitespace-normal">{label}</p>
      {normalizedDescription ? (
        <p className="text-muted-foreground mt-0.5 text-xs leading-snug whitespace-pre-wrap wrap-break-word">
          {normalizedDescription}
        </p>
      ) : null}
    </div>
  )
}
