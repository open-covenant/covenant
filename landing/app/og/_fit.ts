// Sets a card title in Archivo Expanded SemiBold: picks the largest size at
// which the title fits the measure in at most three lines, and breaks the lines
// itself so the renderer never wraps on its own.

// Advance widths and kerning pairs in em for Archivo Expanded SemiBold (wdth
// 125, wght 600), read from the font in _assets with fontTools (kerning from
// its GPOS kern feature). Titles are set in capitals, so only uppercase forms
// are listed.
const ADVANCE: Record<string, number> = {"A":0.908,"B":0.895,"C":0.946,"D":0.934,"E":0.866,"F":0.789,"G":1.013,"H":0.965,"I":0.346,"J":0.699,"K":0.907,"L":0.715,"M":1.112,"N":0.965,"O":1.018,"P":0.856,"Q":1.018,"R":0.92,"S":0.85,"T":0.814,"U":0.944,"V":0.864,"W":1.192,"X":0.914,"Y":0.861,"Z":0.834,"0":0.757,"1":0.67,"2":0.753,"3":0.761,"4":0.736,"5":0.76,"6":0.763,"7":0.691,"8":0.774,"9":0.763," ":0.32,"!":0.318,"\"":0.431,"#":0.739,"$":0.666,"%":1.091,"&":0.925,"'":0.224,"(":0.349,")":0.349,"*":0.431,"+":0.72,",":0.312,"-":0.416,".":0.312,"/":0.306,":":0.312,";":0.312,"<":0.72,"=":0.72,">":0.72,"?":0.681,"@":1.269,"[":0.325,"]":0.325,"_":0.627,"’":0.283,"‘":0.283,"“":0.498,"”":0.498,"·":0.418,"—":1.25,"–":0.625,"×":0.72,"…":0.925};
const FALLBACK_ADVANCE = 0.9;
const KERN: Record<string, number> = {" Y":-0.08,"\",":-0.06,"\".":-0.06,"\"4":-0.01,"\"A":-0.14,"\"J":-0.1,"\"…":-0.06,"',":-0.06,"'.":-0.06,"'4":-0.01,"'A":-0.14,"'J":-0.1,"'…":-0.06,"-0":0.04,"-1":0.04,"-2":0.04,"-3":0.04,"-4":0.04,"-5":0.04,"-6":0.04,"-8":0.03,"-9":0.02,"-V":-0.08,"-W":-0.08,"-X":-0.07,"-Y":-0.18,"/J":-0.02,"0\"":0.01,"0'":0.01,"0-":0.04,"0–":0.04,"0—":0.04,"1\"":-0.04,"1'":-0.04,"1-":0.04,"1–":0.04,"1—":0.04,"2-":0.01,"2–":0.01,"2—":0.01,"3-":0.04,"3–":0.04,"3—":0.04,"4-":0.04,"4–":0.04,"4—":0.04,"5-":0.04,"5–":0.04,"5—":0.04,"6-":0.02,"6–":0.02,"6—":0.02,"7)":-0.01,"7,":-0.14,"7-":-0.04,"7.":-0.14,"77":-0.01,"7]":-0.01,"7–":-0.04,"7—":-0.04,"7…":-0.14,"8-":0.03,"8–":0.03,"8—":0.03,"9-":0.04,"9–":0.04,"9—":0.04,"A\"":-0.14,"A'":-0.14,"A*":-0.12,"A/":0.04,"A?":-0.04,"AC":-0.04,"AG":-0.04,"AO":-0.04,"AQ":-0.04,"AT":-0.135,"AU":-0.015,"AV":-0.06,"AW":-0.06,"AX":-0.03,"AY":-0.115,"A“":-0.1,"A”":-0.1,"BV":-0.01,"BW":-0.01,"BX":-0.015,"BY":-0.02,"CT":-0.03,"CV":-0.005,"CW":-0.005,"CX":-0.01,"CY":-0.02,"D\"":-0.02,"D'":-0.02,"D,":-0.03,"D.":-0.03,"DA":-0.04,"DT":-0.025,"DV":-0.04,"DW":-0.04,"DX":-0.07,"DY":-0.03,"DZ":-0.01,"D…":-0.03,"EJ":-0.03,"F ":-0.03,"F,":-0.06,"F-":-0.03,"F.":-0.06,"F:":-0.01,"F;":-0.01,"FA":-0.07,"FJ":-0.09,"FX":-0.04,"FY":-0.03,"FZ":-0.04,"F–":-0.03,"F—":-0.03,"F…":-0.06,"GV":-0.005,"GW":-0.005,"K ":-0.05,"K\"":-0.01,"K'":-0.01,"K*":-0.04,"K-":-0.08,"K/":0.04,"KA":-0.06,"KC":-0.09,"KG":-0.09,"KJ":-0.05,"KO":-0.09,"KQ":-0.09,"KT":-0.04,"KV":-0.06,"KW":-0.06,"KX":-0.02,"KY":-0.06,"KZ":-0.01,"K·":-0.09,"K–":-0.08,"K—":-0.08,"K“":-0.06,"L\"":-0.16,"L'":-0.16,"L*":-0.1,"L/":0.04,"L?":-0.07,"LC":-0.05,"LG":-0.05,"LJ":-0.03,"LO":-0.05,"LQ":-0.05,"LT":-0.16,"LU":-0.04,"LV":-0.11,"LW":-0.11,"LX":-0.24,"LY":-0.18,"LZ":-0.03,"L·":-0.17,"L’":-0.16,"L“":-0.2,"L”":-0.16,"O\"":-0.02,"O'":-0.02,"O,":-0.03,"O.":-0.03,"OA":-0.04,"OT":-0.025,"OV":-0.04,"OW":-0.04,"OX":-0.07,"OY":-0.03,"OZ":-0.01,"O…":-0.03,"P ":-0.04,"P,":-0.09,"P.":-0.09,"PA":-0.07,"PJ":-0.08,"PT":-0.02,"PX":-0.07,"PY":-0.02,"PZ":-0.03,"P…":-0.09,"Q\"":-0.02,"Q'":-0.02,"Q,":-0.03,"Q.":-0.03,"QA":-0.04,"QT":-0.025,"QV":-0.04,"QW":-0.04,"QX":-0.07,"QY":-0.03,"QZ":-0.01,"Q…":-0.03,"RJ":-0.04,"RT":-0.01,"RU":-0.005,"RV":-0.005,"RW":-0.005,"RX":-0.02,"RY":-0.02,"SY":-0.04,"T,":-0.05,"T.":-0.05,"T/":-0.03,"T:":-0.02,"T;":-0.02,"TA":-0.135,"TC":-0.025,"TG":-0.025,"TJ":-0.14,"TO":-0.025,"TQ":-0.025,"TX":-0.03,"TY":-0.02,"TZ":-0.03,"T…":-0.05,"UA":-0.015,"V,":-0.2,"V-":-0.08,"V.":-0.2,"V:":-0.02,"V;":-0.02,"VA":-0.06,"VC":-0.04,"VG":-0.04,"VJ":-0.11,"VO":-0.04,"VQ":-0.04,"VX":-0.02,"VY":-0.02,"V–":-0.08,"V—":-0.08,"V…":-0.2,"W,":-0.2,"W-":-0.08,"W.":-0.2,"W:":-0.02,"W;":-0.02,"WA":-0.06,"WC":-0.04,"WG":-0.04,"WJ":-0.11,"WO":-0.04,"WQ":-0.04,"WX":-0.02,"WY":-0.02,"W–":-0.08,"W—":-0.08,"W…":-0.2,"X-":-0.07,"XA":-0.03,"XC":-0.07,"XG":-0.07,"XO":-0.07,"XQ":-0.07,"XT":-0.03,"XV":-0.02,"XW":-0.02,"XY":-0.03,"XZ":-0.02,"X·":-0.05,"X–":-0.07,"X—":-0.07,"X“":-0.05,"Y ":-0.08,"Y,":-0.24,"Y-":-0.18,"Y.":-0.24,"Y/":-0.03,"Y:":-0.04,"Y;":-0.04,"YA":-0.115,"YC":-0.03,"YG":-0.03,"YJ":-0.19,"YO":-0.03,"YQ":-0.03,"YS":-0.04,"YT":-0.02,"YV":-0.02,"YW":-0.02,"YX":-0.03,"Y·":-0.05,"Y–":-0.18,"Y—":-0.18,"Y“":-0.05,"Y…":-0.24,"Z-":-0.08,"ZC":-0.01,"ZG":-0.01,"ZO":-0.01,"ZQ":-0.01,"ZT":-0.03,"ZX":-0.02,"Z–":-0.08,"Z—":-0.08,"·X":-0.05,"·Y":-0.05,"–0":0.04,"–1":0.04,"–2":0.04,"–3":0.04,"–4":0.04,"–5":0.04,"–6":0.04,"–8":0.03,"–9":0.02,"–V":-0.08,"–W":-0.08,"–X":-0.07,"–Y":-0.18,"—0":0.04,"—1":0.04,"—2":0.04,"—3":0.04,"—4":0.04,"—5":0.04,"—6":0.04,"—8":0.03,"—9":0.02,"—V":-0.08,"—W":-0.08,"—X":-0.07,"—Y":-0.18,"’A":-0.14,"“A":-0.1,"“J":-0.1,"”,":-0.06,"”.":-0.06,"”…":-0.06};

// Display tracking, as on the site: -0.05em.
export const TITLE_TRACKING = -0.05;
export const TITLE_LEADING = 0.92;

export type FittedTitle = { size: number; lines: string[] };

const kern = (a: string | undefined, b: string | undefined) => (a && b ? (KERN[a + b] ?? 0) : 0);

function measureEm(text: string): number {
  let em = 0;
  let prev: string | undefined;
  for (const ch of text) {
    em += (ADVANCE[ch] ?? FALLBACK_ADVANCE) + TITLE_TRACKING + kern(prev, ch);
    prev = ch;
  }
  return em;
}

export function measure(text: string, size: number): number {
  return measureEm(text) * size;
}

// The image renderer sizes each word by its letters' unkerned widths but
// draws it kerned, so a tightly kerned word such as PAYMENT leaves a wide gap
// after it. The card sets each word separately and puts this margin (in em)
// after it: the word's own kerning, plus the space and its kerning against the
// letters either side.
export function wordSpacing(line: string): Array<{ word: string; after: number }> {
  const words = line.split(" ").filter(Boolean);
  return words.map((word, i) => {
    const chars = [...word];
    let after = measureEm(word) - chars.reduce((em, ch) => em + (ADVANCE[ch] ?? FALLBACK_ADVANCE) + TITLE_TRACKING, 0);
    const next = words[i + 1];
    if (next) after += ADVANCE[" "] + TITLE_TRACKING + kern(chars.at(-1), " ") + kern(" ", [...next][0]);
    return { word, after };
  });
}

function breakLines(words: string[], size: number, width: number): string[] {
  const lines: string[] = [];
  let line = "";
  for (const word of words) {
    const next = line ? `${line} ${word}` : word;
    if (line && measure(next, size) > width) {
      lines.push(line);
      line = word;
    } else {
      line = next;
    }
  }
  if (line) lines.push(line);
  return lines;
}

export function fitTitle(
  title: string,
  { width, maxLines = 3, maxSize = 112, minSize = 48 }: { width: number; maxLines?: number; maxSize?: number; minSize?: number },
): FittedTitle {
  // Typographic apostrophes, as set on the site: ISN’T, not ISN'T.
  const words = title.replace(/(\w)'(\w)/g, "$1’$2").toUpperCase().split(/\s+/).filter(Boolean);
  for (let size = maxSize; size >= minSize; size -= 2) {
    const lines = breakLines(words, size, width);
    if (lines.length <= maxLines && lines.every((l) => measure(l, size) <= width)) {
      return { size, lines };
    }
  }
  // Longer than any card should carry: set it at the smallest size and end the
  // last line with an ellipsis.
  const lines = breakLines(words, minSize, width).slice(0, maxLines);
  let last = lines[maxLines - 1] ?? "";
  if (breakLines(words, minSize, width).length > maxLines) {
    while (last && measure(`${last}…`, minSize) > width) last = last.replace(/\s*\S+$/, "");
    lines[lines.length - 1] = `${last}…`;
  }
  return { size: minSize, lines };
}
