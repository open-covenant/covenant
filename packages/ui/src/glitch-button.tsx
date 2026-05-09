import type { ButtonHTMLAttributes, AnchorHTMLAttributes, ReactNode } from 'react';

type SharedProps = {
  children: ReactNode;
  variant?: 'outline' | 'solid' | 'ghost';
  size?: 'sm' | 'md' | 'lg';
};

type AsButton = SharedProps &
  Omit<ButtonHTMLAttributes<HTMLButtonElement>, keyof SharedProps> & { as?: 'button'; href?: never };
type AsAnchor = SharedProps &
  Omit<AnchorHTMLAttributes<HTMLAnchorElement>, keyof SharedProps> & { as: 'a' };

type Props = AsButton | AsAnchor;

export function GlitchButton({ children, variant = 'outline', size = 'md', ...rest }: Props) {
  const base = [
    'btn-glitch',
    variant !== 'outline' ? `btn-glitch--${variant}` : '',
    size !== 'md' ? `btn-glitch--${size}` : '',
  ]
    .filter(Boolean)
    .join(' ');

  const cls = rest.className ? `${base} ${rest.className}` : base;
  const text = typeof children === 'string' ? children : undefined;

  const inner = (
    <>
      <span className="btn-glitch__label" data-text={text}>{children}</span>
      <span className="btn-glitch__accent btn-glitch__accent--tr" aria-hidden="true" />
      <span className="btn-glitch__accent btn-glitch__accent--bl" aria-hidden="true" />
      <span className="btn-glitch__corner" aria-hidden="true" />
    </>
  );

  if (rest.as === 'a') {
    const { as: _, ...anchorProps } = rest as AsAnchor;
    return <a {...anchorProps} className={cls}>{inner}</a>;
  }

  const { as: _, ...buttonProps } = rest as AsButton;
  return <button type="button" {...buttonProps} className={cls}>{inner}</button>;
}
