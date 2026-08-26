'use client';

import { useEffect, useId, useRef, useState } from 'react';

export type WorkbenchSelectOption = {
  value: string;
  label: string;
  disabled?: boolean;
};

export function WorkbenchSelect({
  id,
  value,
  options,
  placeholder,
  disabled = false,
  labelledBy,
  onChange,
}: {
  id: string;
  value: string;
  options: readonly WorkbenchSelectOption[];
  placeholder: string;
  disabled?: boolean;
  labelledBy: string;
  onChange: (value: string) => void;
}) {
  const listboxId = `${useId()}-listbox`;
  const rootRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const optionRefs = useRef<Array<HTMLButtonElement | null>>([]);
  const [open, setOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(() => selectedIndex(options, value));
  const selected = options.find((option) => option.value === value);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      if (!rootRef.current?.contains(event.target as Node)) setOpen(false);
    };
    document.addEventListener('pointerdown', onPointerDown);
    return () => document.removeEventListener('pointerdown', onPointerDown);
  }, [open]);

  useEffect(() => {
    if (open) optionRefs.current[activeIndex]?.focus();
  }, [activeIndex, open]);

  function show(direction: 1 | -1) {
    if (disabled) return;
    const current = selectedIndex(options, value);
    setActiveIndex(current >= 0 ? current : edgeEnabledIndex(options, direction));
    setOpen(true);
  }

  function choose(option: WorkbenchSelectOption) {
    if (option.disabled) return;
    onChange(option.value);
    setOpen(false);
    requestAnimationFrame(() => triggerRef.current?.focus());
  }

  function move(direction: 1 | -1) {
    setActiveIndex((current) => nextEnabledIndex(options, current, direction));
  }

  return (
    <div className="workbench-select" ref={rootRef}>
      <button
        className="workbench-select-trigger"
        id={id}
        ref={triggerRef}
        type="button"
        disabled={disabled}
        aria-labelledby={`${labelledBy} ${id}`}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? listboxId : undefined}
        onClick={() => (open ? setOpen(false) : show(1))}
        onKeyDown={(event) => {
          if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
            event.preventDefault();
            show(event.key === 'ArrowDown' ? 1 : -1);
          }
        }}
      >
        <span className={selected ? undefined : 'workbench-select-placeholder'}>
          {selected?.label ?? placeholder}
        </span>
        <span className="workbench-select-chevron" aria-hidden="true" />
      </button>

      {open && (
        <div
          className="workbench-select-list"
          id={listboxId}
          role="listbox"
          aria-labelledby={labelledBy}
        >
          {options.map((option, index) => (
            <button
              className="workbench-select-option"
              key={option.value}
              ref={(element) => {
                optionRefs.current[index] = element;
              }}
              type="button"
              role="option"
              aria-selected={option.value === value}
              disabled={option.disabled}
              onClick={() => choose(option)}
              onKeyDown={(event) => {
                if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
                  event.preventDefault();
                  move(event.key === 'ArrowDown' ? 1 : -1);
                } else if (event.key === 'Home' || event.key === 'End') {
                  event.preventDefault();
                  setActiveIndex(edgeEnabledIndex(options, event.key === 'Home' ? 1 : -1));
                } else if (event.key === 'Escape') {
                  event.preventDefault();
                  setOpen(false);
                  triggerRef.current?.focus();
                } else if (event.key === 'Tab') {
                  setOpen(false);
                }
              }}
            >
              <span>{option.label}</span>
              {option.value === value && <span aria-hidden="true">✓</span>}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

function selectedIndex(options: readonly WorkbenchSelectOption[], value: string): number {
  return options.findIndex((option) => option.value === value && !option.disabled);
}

function nextEnabledIndex(
  options: readonly WorkbenchSelectOption[],
  current: number,
  direction: 1 | -1,
): number {
  if (!options.length) return -1;
  for (let offset = 1; offset <= options.length; offset += 1) {
    const index = (current + direction * offset + options.length) % options.length;
    if (!options[index]?.disabled) return index;
  }
  return -1;
}

function edgeEnabledIndex(options: readonly WorkbenchSelectOption[], direction: 1 | -1): number {
  const start = direction > 0 ? -1 : 0;
  return nextEnabledIndex(options, start, direction);
}
