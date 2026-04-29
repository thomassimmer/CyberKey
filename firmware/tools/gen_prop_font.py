import sys

def convert_bdf_to_proportional(bdf_file, output_rs):
    with open(bdf_file, 'r') as f:
        lines = f.readlines()

    glyphs = {}
    fbb_h, fbb_y = 0, 0
    current_char = None
    in_bitmap = False
    bitmap = []
    char_w, char_h, char_x, char_y = 0, 0, 0, 0

    for line in lines:
        line = line.strip()
        if line.startswith('FONTBOUNDINGBOX'):
            _, h, _, y = map(int, line.split()[1:])
            fbb_h, fbb_y = h, y
        elif line.startswith('ENCODING'):
            current_char = int(line.split()[1])
        elif line.startswith('BBX'):
            char_w, char_h, char_x, char_y = map(int, line.split()[1:])
        elif line.startswith('BITMAP'):
            in_bitmap = True
            bitmap = []
        elif line.startswith('ENDCHAR'):
            in_bitmap = False
            if 32 <= current_char <= 126:
                glyphs[current_char] = (bitmap, char_w, char_h, char_x, char_y)
        elif in_bitmap:
            bitmap.append(line)

    if not glyphs:
        print("Erreur : Aucun glyphe trouvé.")
        return

    # On va stocker les données de manière compacte
    # Chaque glyphe sera stocké ligne par ligne, mais seulement sur sa largeur réelle
    all_data = bytearray()
    glyph_info = [] # (offset_in_all_data, width, height, x_offset, y_offset)
    
    current_offset = 0
    baseline = fbb_h + fbb_y

    for code in range(32, 127):
        if code in glyphs:
            bm, w, h, x, y = glyphs[code]
            
            # On stocke le bitmap du glyphe
            # Pour simplifier le dessin, on stocke chaque ligne alignée sur l'octet
            bytes_per_row = (w + 7) // 8
            glyph_data = bytearray(bytes_per_row * h)
            
            for row_idx, hex_str in enumerate(bm):
                val = int(hex_str, 16)
                bits_in_hex = len(hex_str) * 4
                row_val = (val >> (bits_in_hex - w)) << (8 - (w % 8) if w % 8 != 0 else 0)
                # En fait, plus simple : on garde le MSB
                val_msb = val >> (bits_in_hex - (bytes_per_row * 8))
                
                for b in range(bytes_per_row):
                    glyph_data[row_idx * bytes_per_row + b] = (val_msb >> ((bytes_per_row - 1 - b) * 8)) & 0xFF
            
            glyph_info.append((current_offset, w, h, x, y))
            all_data.extend(glyph_data)
            current_offset += len(glyph_data)
        else:
            # Caractère manquant (espace par défaut ou vide)
            glyph_info.append((0, 5, 0, 0, 0))

    # Génération du fichier Rust
    with open(output_rs, 'w') as f:
        f.write("// GÉNERÉ AUTOMATIQUEMENT PAR gen_prop_font.py\n")
        f.write("use embedded_graphics::{prelude::*, pixelcolor::Rgb565};\n\n")
        f.write(f"pub const FONT_HEIGHT: i32 = {fbb_h};\n")
        f.write(f"pub const BASELINE: i32 = {baseline};\n\n")
        f.write(f"const FONT_DATA: &[u8] = &{list(all_data)};\n\n")
        f.write("// (offset, width, height, x_offset, y_offset)\n")
        f.write(f"const GLYPH_INFO: [(u32, u8, u8, i8, i8); 95] = {glyph_info};\n\n")
        f.write("""
pub fn draw_text_prop<D>(
    display: &mut D,
    text: &str,
    position: Point,
    color: Rgb565,
) -> Result<Point, D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let mut cursor = position;

    for c in text.chars() {
        let code = c as u32;
        if code < 32 || code > 126 { continue; }
        
        let (offset, w, h, x, y) = GLYPH_INFO[(code - 32) as usize];
        if w == 0 { 
            cursor.x += 5; // Espace par défaut
            continue; 
        }

        let bytes_per_row = (w as usize + 7) / 8;
        let glyph_data = &FONT_DATA[offset as usize .. (offset as usize + bytes_per_row * h as usize)];
        
        // Calcul de la position Y pour l'alignement sur la baseline
        // baseline = 17. Si y=-4 et h=15, top = 17 - (15-4) = 6.
        let top = cursor.y + (BASELINE - (h as i32 + y as i32));
        let left = cursor.x + x as i32;

        for row in 0..h as i32 {
            for col in 0..w as i32 {
                let byte_idx = (col as usize) / 8;
                let bit_idx = 7 - (col % 8);
                let byte = glyph_data[row as usize * bytes_per_row + byte_idx];
                
                if (byte & (1 << bit_idx)) != 0 {
                    display.draw_iter(core::iter::once(Pixel(Point::new(left + col, top + row), color)))?;
                }
            }
        }
        cursor.x += (w as i32 + x as i32 + 1); // +1 pour l'espacement entre lettres
    }
    Ok(cursor)
}
""")
    
    print(f"Fichier généré : {output_rs}")

if __name__ == '__main__':
    if len(sys.argv) < 3:
        print("Usage: python3 gen_prop_font.py <input.bdf> <output.rs>")
    else:
        convert_bdf_to_proportional(sys.argv[1], sys.argv[2])
