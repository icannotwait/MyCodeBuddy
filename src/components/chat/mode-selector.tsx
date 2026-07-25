"use client"

import { useState } from "react"
import { ChevronDown } from "lucide-react"
import { Button } from "@/components/ui/button"
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu"
import { DropdownRadioItemContent } from "@/components/chat/dropdown-radio-item-content"
import type { SessionModeInfo } from "@/lib/types"

interface ModeSelectorProps {
  modes: SessionModeInfo[]
  selectedModeId: string | null
  onSelect: (modeId: string) => void
  label: string
  /** When true, trigger stays visible with the current value but cannot open. */
  disabled?: boolean
}

export function InlineModeSelector({
  modes,
  selectedModeId,
  onSelect,
  label,
  disabled = false,
}: ModeSelectorProps) {
  const [open, setOpen] = useState(false)
  // Close on relock without an effect (React render-time prop→state adjust).
  const [wasDisabled, setWasDisabled] = useState(disabled)
  if (disabled !== wasDisabled) {
    setWasDisabled(disabled)
    if (disabled) setOpen(false)
  }
  const selected = modes.find((mode) => mode.id === selectedModeId)
  const currentLabel = selected?.name ?? selectedModeId ?? ""
  return (
    <DropdownMenu
      open={open}
      onOpenChange={(next) => {
        if (disabled) {
          setOpen(false)
          return
        }
        setOpen(next)
      }}
    >
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="xs"
          disabled={disabled}
          title={label}
          aria-label={currentLabel ? `${label}: ${currentLabel}` : label}
          className="min-w-0 gap-0.5 px-1 text-muted-foreground"
        >
          <span className="max-w-[10rem] truncate">{currentLabel}</span>
          <ChevronDown className="size-3 shrink-0 text-muted-foreground" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent
        side="top"
        align="start"
        className="max-h-[60vh] min-w-72 overflow-y-auto"
        style={{
          maxWidth: "min(20rem, calc(100vw - 1rem))",
        }}
      >
        <DropdownMenuRadioGroup
          value={selectedModeId ?? ""}
          onValueChange={onSelect}
        >
          {modes.map((mode) => (
            <DropdownMenuRadioItem key={mode.id} value={mode.id}>
              <DropdownRadioItemContent
                label={mode.name}
                description={mode.description}
              />
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
