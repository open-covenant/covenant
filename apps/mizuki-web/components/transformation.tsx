const steps = [
  { number: '01', label: 'Paid attempt', detail: 'A maintainer authorizes one bounded issue.' },
  {
    number: '02',
    label: 'Full refund',
    detail: 'If validation fails, the original payer gets every cent back.',
  },
  {
    number: '03',
    label: 'Funded rescue',
    detail: 'The failure becomes a public, independently escrowed bounty.',
  },
  {
    number: '04',
    label: 'New capability',
    detail: 'The merged fix becomes evidence Mizuki can use to improve himself.',
  },
];

export function Transformation() {
  return (
    <ol className="transformation" aria-label="How a failed job becomes a capability">
      {steps.map((step, index) => (
        <li key={step.number}>
          <span className="transformation-number">{step.number}</span>
          <div>
            <strong>{step.label}</strong>
            <p>{step.detail}</p>
          </div>
          {index < steps.length - 1 && (
            <span className="transformation-arrow" aria-hidden="true">
              →
            </span>
          )}
        </li>
      ))}
    </ol>
  );
}
