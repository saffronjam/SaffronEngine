//! Yale Bright Star Catalog fixed-width parser and compact runtime-table baker.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

const MAGIC: [u8; 8] = *b"SABSC5\0\x01";

pub fn bake(source: &Path, output: &Path) -> Result<usize> {
    let catalog = fs::read_to_string(source)
        .with_context(|| format!("read Yale BSC5 source '{}'", source.display()))?;
    let mut records = Vec::new();
    for (line_index, line) in catalog.lines().enumerate() {
        if line.len() < 147 {
            continue;
        }
        if let Some(record) = parse_record(line.as_bytes())
            .with_context(|| format!("parse Yale BSC5 source line {}", line_index + 1))?
        {
            records.push(record);
        }
    }
    if records.len() < 9_000 {
        bail!(
            "Yale BSC5 source yielded only {} stars (expected at least 9000)",
            records.len()
        );
    }

    let mut bytes = Vec::with_capacity(12 + records.len() * 32);
    bytes.extend_from_slice(&MAGIC);
    bytes.extend_from_slice(&(records.len() as u32).to_le_bytes());
    for record in records {
        for value in record {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create star asset directory '{}'", parent.display()))?;
    }
    fs::write(output, bytes)
        .with_context(|| format!("write baked star catalog '{}'", output.display()))?;
    Ok((fs::metadata(output)?.len() as usize - 12) / 32)
}

fn parse_record(line: &[u8]) -> Result<Option<[f32; 8]>> {
    let Some(ra_h) = field(line, 75, 77).parse::<f64>().ok() else {
        return Ok(None);
    };
    let Some(ra_m) = field(line, 77, 79).parse::<f64>().ok() else {
        return Ok(None);
    };
    let Some(ra_s) = field(line, 79, 83).parse::<f64>().ok() else {
        return Ok(None);
    };
    let Some(dec_d) = field(line, 84, 86).parse::<f64>().ok() else {
        return Ok(None);
    };
    let Some(dec_m) = field(line, 86, 88).parse::<f64>().ok() else {
        return Ok(None);
    };
    let Some(dec_s) = field(line, 88, 90).parse::<f64>().ok() else {
        return Ok(None);
    };
    let Some(magnitude) = field(line, 102, 107).parse::<f64>().ok() else {
        return Ok(None);
    };
    let sign = match line[83] {
        b'-' => -1.0,
        b'+' | b' ' => 1.0,
        other => bail!("invalid declination sign byte {other}"),
    };
    let right_ascension = (ra_h + ra_m / 60.0 + ra_s / 3600.0) * 15.0_f64.to_radians();
    let declination = sign * (dec_d + dec_m / 60.0 + dec_s / 3600.0).to_radians();
    let cos_declination = declination.cos();
    let direction = [
        (cos_declination * right_ascension.cos()) as f32,
        (cos_declination * right_ascension.sin()) as f32,
        declination.sin() as f32,
    ];
    let luminance = 2.512_f64.powf(7.0 - magnitude) as f32;
    let temperature = spectral_temperature(field(line, 127, 147));
    let rgb = blackbody_linear_srgb(temperature);
    Ok(Some([
        direction[0],
        direction[1],
        direction[2],
        luminance,
        rgb[0],
        rgb[1],
        rgb[2],
        0.0,
    ]))
}

fn field(line: &[u8], begin: usize, end: usize) -> &str {
    std::str::from_utf8(&line[begin..end])
        .unwrap_or_default()
        .trim()
}

fn spectral_temperature(spectral: &str) -> f64 {
    let mut chars = spectral.chars();
    let class = chars.next().unwrap_or('F').to_ascii_uppercase();
    let subtype = chars
        .next()
        .and_then(|value| value.to_digit(10))
        .map_or(5.0, f64::from)
        / 10.0;
    let (hot, cool) = match class {
        'O' => (40_000.0, 30_000.0),
        'B' => (30_000.0, 10_000.0),
        'A' => (10_000.0, 7_500.0),
        'F' => (7_500.0, 6_000.0),
        'G' => (6_000.0, 5_200.0),
        'K' => (5_200.0, 3_700.0),
        'M' => (3_700.0, 2_400.0),
        _ => (6_500.0, 6_500.0),
    };
    hot + (cool - hot) * subtype
}

fn blackbody_linear_srgb(temperature: f64) -> [f32; 3] {
    let t = temperature.clamp(1_667.0, 25_000.0);
    let x = if t <= 4_000.0 {
        -0.266_123_9e9 / t.powi(3) - 0.234_358e6 / t.powi(2) + 0.877_695_6e3 / t + 0.179_91
    } else {
        -3.025_846_9e9 / t.powi(3) + 2.107_037_9e6 / t.powi(2) + 0.222_634_7e3 / t + 0.240_39
    };
    let y = if t <= 2_222.0 {
        -1.106_381_4 * x.powi(3) - 1.348_110_2 * x.powi(2) + 2.185_558_32 * x - 0.202_196_83
    } else if t <= 4_000.0 {
        -0.954_947_6 * x.powi(3) - 1.374_185_93 * x.powi(2) + 2.091_370_15 * x - 0.167_488_67
    } else {
        3.081_758 * x.powi(3) - 5.873_386_7 * x.powi(2) + 3.751_129_97 * x - 0.370_014_83
    };
    let xyz = [x / y, 1.0, (1.0 - x - y) / y];
    let mut rgb = [
        3.240_454_2 * xyz[0] - 1.537_138_5 * xyz[1] - 0.498_531_4 * xyz[2],
        -0.969_266 * xyz[0] + 1.876_010_8 * xyz[1] + 0.041_556 * xyz[2],
        0.055_643_4 * xyz[0] - 0.204_025_9 * xyz[1] + 1.057_225_2 * xyz[2],
    ];
    for channel in &mut rgb {
        *channel = channel.max(0.0);
    }
    let peak = rgb[0].max(rgb[1]).max(rgb[2]).max(1.0e-9);
    [
        (rgb[0] / peak) as f32,
        (rgb[1] / peak) as f32,
        (rgb[2] / peak) as f32,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blackbody_is_blue_when_hot_and_red_when_cool() {
        let hot = blackbody_linear_srgb(20_000.0);
        let cool = blackbody_linear_srgb(2_500.0);
        assert!(hot[2] > hot[0]);
        assert!(cool[0] > cool[2]);
    }
}
