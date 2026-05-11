import type { HTMLAttributes } from 'react';

type Props = HTMLAttributes<HTMLDivElement> & {
  width?: string;
  height?: string;
};

export function Skeleton({ width, height = '1em', className, style, ...rest }: Props) {
  return (
    <div
      aria-hidden="true"
      {...rest}
      className={['animate-pulse bg-[var(--ink)]/[0.08] rounded-sm', className].filter(Boolean).join(' ')}
      style={{ width, height, ...style }}
    />
  );
}
