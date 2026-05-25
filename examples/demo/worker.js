// Worker entry. Loads the wasm module once (the wasm-pack `--target
// web` bundle exports `default` as the init function and pulls the
// `.wasm` from the same directory) and dispatches scenario runs.
//
// The three scenarios mirror `crates/xl3-core/examples/bench.rs`
// so the browser path can be compared directly against the native
// numbers in BENCH.md.

import init, { convert } from '../../crates/xl3-wasm/pkg/xl3_wasm.js';

const ready = init();

const scenarios = {
  'wide-flat': buildWideFlat,
  'multi-sheet': buildMultiSheet,
  'multi-source-join': buildMultiSourceJoin,
};

self.addEventListener('message', async (event) => {
  const { id, scenario } = event.data;
  try {
    await ready;
    const build = scenarios[scenario];
    if (!build) throw new Error(`unknown scenario: ${scenario}`);
    const { template, data } = build();
    const samples = [];
    for (let i = 0; i < 3; i++) {
      const t0 = performance.now();
      const outputs = convert(template, data, {});
      if (!outputs || outputs.length === 0) {
        throw new Error('convert produced no outputs');
      }
      samples.push(performance.now() - t0);
    }
    samples.sort((a, b) => a - b);
    self.postMessage({ id, ok: true, median: samples[1], samples });
  } catch (e) {
    self.postMessage({ id, ok: false, error: e?.message ?? String(e) });
  }
});

// -----------------------------------------------------------------
// XLSX builders. We don't depend on exceljs to keep the demo's
// runtime cost honest (the workbook generation happens once before
// the timed loop). The minimal-writer below produces the same five
// pieces of an OOXML xlsx that calamine + rust_xlsxwriter accept on
// the read path; it's deflate-only, ASCII-only, no styles.
// -----------------------------------------------------------------

const enc = new TextEncoder();

function buildWideFlat() {
  const template = buildTemplate({
    config: [
      ['name', 'wide-flat'],
      ['source_sheet', 'Data'],
      ['source_table', '1'],
      ['output_file_pattern', 'wide.xlsx'],
    ],
    sheets: [
      {
        name: 'Out',
        rows: [
          ['Account', 'Region', 'Amount', 'Tier'],
          ['{{ [Account] }}', '{{ [Region] }}', '{{ ROUND([Amount], 2) }}', '{{ IF([Amount] > 10000, "Priority", "Standard") }}'],
        ],
      },
    ],
  });
  const data = buildDataSheet('Data', ['Account', 'Region', 'Amount'], (i) => [
    `Acct-${i}`,
    i % 5 === 0 ? 'Seoul' : 'Busan',
    (i * 7) % 30000,
  ], 10000);
  return { template, data };
}

function buildMultiSheet() {
  const template = buildTemplate({
    config: [
      ['name', 'multi-sheet'],
      ['source_sheet', 'Data'],
      ['source_table', '1'],
      ['output_file_pattern', 'multi.xlsx'],
    ],
    sheets: [
      {
        name: '{{ Region }}',
        rows: [
          ['Account', 'Amount'],
          ['{{ [Account] }}', '{{ [Amount] }}'],
        ],
      },
    ],
  });
  const regions = ['Seoul', 'Busan', 'Daegu', 'Incheon', 'Jeju'];
  const data = buildDataSheet('Data', ['Account', 'Region', 'Amount'], (i) => [
    `A${i}`,
    regions[i % regions.length],
    i,
  ], 5000);
  return { template, data };
}

function buildMultiSourceJoin() {
  const template = buildTemplate({
    config: [
      ['name', 'multi-source-join'],
      ['source_sheet', 'Renewals'],
      ['source_table', '1'],
      ['output_file_pattern', 'join.xlsx'],
    ],
    sources: [
      ['Renewals', 'Renewals', '1'],
      ['Customers', 'Customers', '1'],
    ],
    sheets: [
      {
        name: 'Out',
        rows: [
          ['Account', 'Region', 'Amount'],
          ['{{ @source Renewals }}', '', ''],
          ['{{ @join Customers on Customers[Account] = Renewals[Account] }}', '', ''],
          ['{{ Renewals[Account] }}', '{{ Customers[Region] }}', '{{ Renewals[Amount] }}'],
        ],
      },
    ],
  });
  const data = buildMultiSheetData([
    { name: 'Customers', headers: ['Account', 'Region'], rowCount: 1000, gen: (i) => [`A${i}`, i % 2 === 0 ? 'Seoul' : 'Busan'] },
    { name: 'Renewals', headers: ['Account', 'Amount'], rowCount: 5000, gen: (i) => [`A${i % 1250}`, i] },
  ]);
  return { template, data };
}

// -----------------------------------------------------------------
// Minimal xlsx writer — five XML parts + the relationships
// scaffolding, store-only zip. Just enough to round-trip through
// calamine; rust_xlsxwriter then writes the *output* properly.
// -----------------------------------------------------------------

function buildTemplate({ config, sheets, sources }) {
  const allSheets = [{ name: '__config__', rows: [['key', 'value'], ...config] }];
  if (sources && sources.length > 0) {
    allSheets.push({
      name: '__sources__',
      rows: [['name', 'sheet', 'table'], ...sources],
    });
  }
  allSheets.push(...sheets);
  return makeXlsx(allSheets);
}

function buildDataSheet(name, headers, rowGen, count) {
  const rows = [headers];
  for (let i = 0; i < count; i++) rows.push(rowGen(i));
  return makeXlsx([{ name, rows }]);
}

function buildMultiSheetData(specs) {
  const sheets = specs.map(({ name, headers, rowCount, gen }) => {
    const rows = [headers];
    for (let i = 0; i < rowCount; i++) rows.push(gen(i));
    return { name, rows };
  });
  return makeXlsx(sheets);
}

function makeXlsx(sheets) {
  const sst = new SharedStrings();
  // workbook.xml.sheetN — index-1 based ids per OOXML.
  const sheetParts = sheets.map((sheet, i) => {
    const rowsXml = sheet.rows
      .map((row, rIdx) => {
        const cells = row
          .map((value, cIdx) => cellXml(value, rIdx + 1, colLetter(cIdx + 1), sst))
          .filter(Boolean)
          .join('');
        return `<row r="${rIdx + 1}">${cells}</row>`;
      })
      .join('');
    const xml =
      `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>` +
      `<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">` +
      `<sheetData>${rowsXml}</sheetData></worksheet>`;
    return { id: i + 1, name: sheet.name, xml };
  });
  const workbookXml =
    `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>` +
    `<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"` +
    ` xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">` +
    `<sheets>${sheetParts
      .map((s) => `<sheet name="${escapeXml(s.name)}" sheetId="${s.id}" r:id="rId${s.id}"/>`)
      .join('')}</sheets></workbook>`;
  const workbookRelsXml =
    `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>` +
    `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">` +
    sheetParts
      .map(
        (s) =>
          `<Relationship Id="rId${s.id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet${s.id}.xml"/>`,
      )
      .join('') +
    `<Relationship Id="rIdSst" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>` +
    `</Relationships>`;
  const contentTypesXml =
    `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>` +
    `<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">` +
    `<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>` +
    `<Default Extension="xml" ContentType="application/xml"/>` +
    `<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>` +
    sheetParts
      .map(
        (s) =>
          `<Override PartName="/xl/worksheets/sheet${s.id}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>`,
      )
      .join('') +
    `<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>` +
    `</Types>`;
  const rootRelsXml =
    `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>` +
    `<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">` +
    `<Relationship Id="rIdWb" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>` +
    `</Relationships>`;
  const files = [
    ['[Content_Types].xml', enc.encode(contentTypesXml)],
    ['_rels/.rels', enc.encode(rootRelsXml)],
    ['xl/workbook.xml', enc.encode(workbookXml)],
    ['xl/_rels/workbook.xml.rels', enc.encode(workbookRelsXml)],
    ['xl/sharedStrings.xml', enc.encode(sst.toXml())],
    ...sheetParts.map((s) => [`xl/worksheets/sheet${s.id}.xml`, enc.encode(s.xml)]),
  ];
  return storeZip(files);
}

function cellXml(value, row, col, sst) {
  if (value === '' || value === null || value === undefined) return '';
  const ref = `${col}${row}`;
  if (typeof value === 'number' && Number.isFinite(value)) {
    return `<c r="${ref}"><v>${value}</v></c>`;
  }
  if (typeof value === 'boolean') {
    return `<c r="${ref}" t="b"><v>${value ? 1 : 0}</v></c>`;
  }
  const idx = sst.intern(String(value));
  return `<c r="${ref}" t="s"><v>${idx}</v></c>`;
}

class SharedStrings {
  constructor() {
    this.map = new Map();
    this.list = [];
  }
  intern(s) {
    if (this.map.has(s)) return this.map.get(s);
    const idx = this.list.length;
    this.map.set(s, idx);
    this.list.push(s);
    return idx;
  }
  toXml() {
    const items = this.list.map((s) => `<si><t xml:space="preserve">${escapeXml(s)}</t></si>`).join('');
    return (
      `<?xml version="1.0" encoding="UTF-8" standalone="yes"?>` +
      `<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" count="${this.list.length}" uniqueCount="${this.list.length}">${items}</sst>`
    );
  }
}

function colLetter(n) {
  let s = '';
  while (n > 0) {
    const r = (n - 1) % 26;
    s = String.fromCharCode(65 + r) + s;
    n = (n - 1 - r) / 26;
  }
  return s;
}

function escapeXml(s) {
  return String(s)
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&apos;');
}

// -----------------------------------------------------------------
// Store-only zip (no compression). The reader (calamine + zip
// crate) accepts both store and deflate; store is simpler and fast
// enough for the demo data sizes (<5 MB).
// -----------------------------------------------------------------

function storeZip(files) {
  const localParts = [];
  const centralParts = [];
  let offset = 0;
  for (const [name, body] of files) {
    const nameBytes = enc.encode(name);
    const crc = crc32(body);
    const local = new Uint8Array(30 + nameBytes.length + body.length);
    const dv = new DataView(local.buffer);
    dv.setUint32(0, 0x04034b50, true);
    dv.setUint16(4, 20, true); // version
    dv.setUint16(6, 0, true); // flags
    dv.setUint16(8, 0, true); // method (store)
    dv.setUint16(10, 0, true); // mod time
    dv.setUint16(12, 0x0021, true); // mod date 1996-01-01
    dv.setUint32(14, crc, true);
    dv.setUint32(18, body.length, true);
    dv.setUint32(22, body.length, true);
    dv.setUint16(26, nameBytes.length, true);
    dv.setUint16(28, 0, true);
    local.set(nameBytes, 30);
    local.set(body, 30 + nameBytes.length);
    localParts.push(local);

    const central = new Uint8Array(46 + nameBytes.length);
    const cdv = new DataView(central.buffer);
    cdv.setUint32(0, 0x02014b50, true);
    cdv.setUint16(4, 20, true);
    cdv.setUint16(6, 20, true);
    cdv.setUint16(8, 0, true);
    cdv.setUint16(10, 0, true);
    cdv.setUint16(12, 0, true);
    cdv.setUint16(14, 0x0021, true);
    cdv.setUint32(16, crc, true);
    cdv.setUint32(20, body.length, true);
    cdv.setUint32(24, body.length, true);
    cdv.setUint16(28, nameBytes.length, true);
    cdv.setUint16(30, 0, true);
    cdv.setUint16(32, 0, true);
    cdv.setUint16(34, 0, true);
    cdv.setUint16(36, 0, true);
    cdv.setUint32(38, 0, true);
    cdv.setUint32(42, offset, true);
    central.set(nameBytes, 46);
    centralParts.push(central);
    offset += local.length;
  }
  const centralOffset = offset;
  const centralBytes = concat(centralParts);
  const end = new Uint8Array(22);
  const edv = new DataView(end.buffer);
  edv.setUint32(0, 0x06054b50, true);
  edv.setUint16(8, files.length, true);
  edv.setUint16(10, files.length, true);
  edv.setUint32(12, centralBytes.length, true);
  edv.setUint32(16, centralOffset, true);
  const total = concat([concat(localParts), centralBytes, end]);
  return total;
}

function concat(chunks) {
  let total = 0;
  for (const c of chunks) total += c.length;
  const out = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) {
    out.set(c, off);
    off += c.length;
  }
  return out;
}

// CRC-32 (IEEE 802.3 polynomial) — required by zip's directory
// entry headers. Table cached so cell-by-cell encoding stays fast.
const CRC32_TABLE = (() => {
  const t = new Uint32Array(256);
  for (let i = 0; i < 256; i++) {
    let c = i;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[i] = c >>> 0;
  }
  return t;
})();

function crc32(bytes) {
  let c = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) {
    c = CRC32_TABLE[(c ^ bytes[i]) & 0xff] ^ (c >>> 8);
  }
  return (c ^ 0xffffffff) >>> 0;
}
