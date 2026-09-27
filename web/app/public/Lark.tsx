// Traced from apple/IconConcepts/Alondra.icon (d-modular-lark-v3); wings pivot on their hinges.
export function Lark({ className = '', label }: { className?: string; label?: string }) {
  const a11y = label ? { role: 'img', 'aria-label': label } : { 'aria-hidden': true };
  return (
    <svg className={`lark ${className}`} viewBox="0 0 880 600" focusable="false" {...a11y}>
      <g className="lark-wing lark-wing-l">
        <path fillRule="evenodd" d="M16 23L23 25L114 78L324 205L334 216L352 254L361 261L366 270L367 287L365 293L358 303L349 308L349 311L370 368L163 300L139 289L131 280L89 212L76 185L280 263L300 270L305 270L305 268L69 157L58 141L16 28ZM336 262L324 268L320 276L320 283L326 294L334 298L342 298L354 290L356 276L350 266Z"/>
      </g>
      <g className="lark-wing lark-wing-r">
        <path fillRule="evenodd" d="M856 23L858 24L856 35L816 142L804 158L569 268L569 270L578 269L798 185L794 197L770 237L742 282L735 289L723 296L505 368L526 311L526 308L521 306L510 292L509 272L516 259L523 255L541 216L552 204L762 77ZM535 262L522 269L519 284L528 296L541 298L552 291L555 277L553 271L547 265Z"/>
      </g>
      <g className="lark-body">
        <path d="M474 145L564 149L505 190L497 204L498 220L510 255L503 269L502 287L509 303L519 311L518 315L498 366L368 493L335 522L374 393L382 383L356 313L368 301L373 290L374 276L371 266L444 161L456 151Z"/>
        <path d="M452 419L454 419L454 423L448 444L421 524L323 584L340 527Z"/>
      </g>
      <circle className="lark-hinge lark-hinge-l" cx="338.1" cy="280.2" r="17.8"/>
      <circle className="lark-hinge lark-hinge-r" cx="536.9" cy="280.1" r="17.8"/>
    </svg>
  );
}
