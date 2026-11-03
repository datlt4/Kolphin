#!/usr/bin/env python3
"""Build the blank documents behind New Document (assets/templates/).

blank.docx and blank.pptx are python-docx / python-pptx's default templates (MIT) with their metadata cleared;
pass the two files as arguments. The others are minimal packages written here.
"""
import re, sys, zipfile
from pathlib import Path

OUT = Path(__file__).resolve().parent.parent / "assets" / "templates"
CORE = ('<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n<cp:coreProperties '
        'xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" '
        'xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" '
        'xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance"><cp:revision>1</cp:revision></cp:coreProperties>')
FIXED = (1980, 1, 1, 0, 0, 0)


def write(name, parts, stored_first=None):
    with zipfile.ZipFile(OUT / name, "w", zipfile.ZIP_DEFLATED) as z:
        if stored_first:
            z.writestr(zipfile.ZipInfo(stored_first[0], FIXED), stored_first[1], zipfile.ZIP_STORED)
        for path, data in parts:
            z.writestr(zipfile.ZipInfo(path, FIXED), data, zipfile.ZIP_DEFLATED)


def ooxml_from(src, name):
    """Copy an OOXML package, dropping the thumbnail and blanking author metadata."""
    with zipfile.ZipFile(src) as z:
        parts = []
        for info in z.infolist():
            data = z.read(info.filename)
            if info.filename == "docProps/thumbnail.jpeg":
                continue
            if info.filename == "docProps/core.xml":
                data = CORE.encode()
            if info.filename in ("[Content_Types].xml", "_rels/.rels"):
                data = re.sub(rb'<(Override|Relationship)[^>]*thumbnail[^>]*/>', b"", data)
            parts.append((info.filename, data))
    write(name, parts)


S = "http://schemas.openxmlformats.org/spreadsheetml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
XLSX = [
    ("[Content_Types].xml", '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
     '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
     '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
     '<Default Extension="xml" ContentType="application/xml"/>'
     '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
     '<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
     '<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>'
     '</Types>'),
    ("_rels/.rels", '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
     '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
     f'<Relationship Id="rId1" Type="{R}/officeDocument" Target="xl/workbook.xml"/></Relationships>'),
    ("xl/workbook.xml", f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n<workbook xmlns="{S}" xmlns:r="{R}">'
     '<sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets></workbook>'),
    ("xl/_rels/workbook.xml.rels", '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n'
     '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
     f'<Relationship Id="rId1" Type="{R}/worksheet" Target="worksheets/sheet1.xml"/>'
     f'<Relationship Id="rId2" Type="{R}/styles" Target="styles.xml"/></Relationships>'),
    ("xl/worksheets/sheet1.xml", f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n<worksheet xmlns="{S}"><sheetData/></worksheet>'),
    ("xl/styles.xml", f'<?xml version="1.0" encoding="UTF-8" standalone="yes"?>\n<styleSheet xmlns="{S}">'
     '<fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts>'
     '<fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills>'
     '<borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders>'
     '<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>'
     '<cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/></cellXfs>'
     '<cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles></styleSheet>'),
]

NS = ('xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" '
      'xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" '
      'xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" '
      'xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" '
      'xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" '
      'xmlns:fo="urn:oasis:names:tc:opendocument:xmlns:xsl-fo-compatible:1.0" '
      'xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" office:version="1.2"')


def odf(name, kind, body, master=""):
    mime = f"application/vnd.oasis.opendocument.{kind}"
    manifest = ('<?xml version="1.0" encoding="UTF-8"?>\n<manifest:manifest '
                'xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0" manifest:version="1.2">'
                f'<manifest:file-entry manifest:full-path="/" manifest:version="1.2" manifest:media-type="{mime}"/>'
                '<manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/>'
                '<manifest:file-entry manifest:full-path="styles.xml" manifest:media-type="text/xml"/>'
                '</manifest:manifest>')
    content = f'<?xml version="1.0" encoding="UTF-8"?>\n<office:document-content {NS}><office:body>{body}</office:body></office:document-content>'
    styles = f'<?xml version="1.0" encoding="UTF-8"?>\n<office:document-styles {NS}>{master}</office:document-styles>'
    write(name, [("META-INF/manifest.xml", manifest), ("content.xml", content), ("styles.xml", styles)], ("mimetype", mime))


OUT.mkdir(parents=True, exist_ok=True)
ooxml_from(sys.argv[1], "blank.docx")
ooxml_from(sys.argv[2], "blank.pptx")
write("blank.xlsx", XLSX)
odf("blank.odt", "text", "<office:text><text:p/></office:text>")
odf("blank.ods", "spreadsheet", '<office:spreadsheet><table:table table:name="Sheet1"><table:table-column/>'
    "<table:table-row><table:table-cell/></table:table-row></table:table></office:spreadsheet>")
odf("blank.odp", "presentation",
    '<office:presentation><draw:page draw:name="page1" draw:master-page-name="Default"/></office:presentation>',
    '<office:automatic-styles><style:page-layout style:name="PM1"><style:page-layout-properties fo:page-width="28cm" '
    'fo:page-height="15.75cm" style:print-orientation="landscape"/></style:page-layout></office:automatic-styles>'
    '<office:master-styles><style:master-page style:name="Default" style:page-layout-name="PM1"/></office:master-styles>')
