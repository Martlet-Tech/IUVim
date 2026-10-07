# TSF 托盘/语言栏图标生成器：品牌素材 → res/{icon,zh,en}.ico。
#
# 设计（2026-10-07，对齐主流输入法）：品牌 logo 用色块，中英态用裸字形无底色
# （参考搜狗/QQ/微软拼音：托盘里 logo 是色块、中/英是裸字）：
#   - icon.ico：品牌蓝圆角方块 + 白色 iuv 字标（assets/main.png 反白）→ DLL/输入指示器主图标
#   - zh.ico  ：品牌蓝裸 中（assets/toolbar-icons/lang-cn.png 原色，无底）→ 语言栏中文态
#   - en.ico  ：品牌蓝裸 A （assets/toolbar-icons/lang-en.png 原色，无底）→ 语言栏英文态
# 裸字形用品牌蓝 #38A6F5：深/浅任务栏都可读，且与工具栏图标完全同款。
# 产物为经典 32bpp DIB 条目（BITMAPINFOHEADER + BGRA XOR + 全零 AND 掩码），
# LoadImageW 全兼容——格式与 convert-main-icon.ps1 相同，本脚本覆盖全部三个 ico。
#
# 用法：python scripts/gen-tsf-icons.py   （需 Pillow）

from __future__ import annotations

from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"
RES = ROOT / "platforms" / "windows" / "iuv-tsf" / "res"

BRAND_BLUE = (56, 166, 245, 255)   # #38A6F5，取自 toolbar-icons 实测
WHITE = (255, 255, 255, 255)
TILE_RADIUS = 0.22                 # 圆角半径占边长比例（icon.ico 色块）
GLYPH_RATIO = 0.92                 # 裸字形（中/A）最大边占画布比例（无底色，占满些）
WORD_RATIO = 0.86                  # 横向字标（iuv）占色块边长比例

SS = 8  # 超采样倍率：先画大图再缩小，圆角与小尺寸字形抗锯齿


def crop_alpha(path: Path) -> Image.Image:
    """载入素材，裁到内容 bbox，返回仅含 alpha 通道的 L 模式图。"""
    im = Image.open(path).convert("RGBA")
    a = im.getchannel("A")
    return a.crop(a.getbbox())


def recolor(alpha: Image.Image, color: tuple[int, int, int, int]) -> Image.Image:
    """把 alpha 蒙版填成单色（素材是纯色字形，反白即得白字形）。"""
    out = Image.new("RGBA", alpha.size, color)
    out.putalpha(alpha)
    return out


def tile(glyph: Image.Image, size: int, glyph_ratio: float) -> Image.Image:
    """蓝底圆角方块 + 居中白字形；超采样绘制后 LANCZOS 缩到目标尺寸。"""
    big = size * SS
    im = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    d = ImageDraw.Draw(im)
    d.rounded_rectangle(
        [0, 0, big - 1, big - 1],
        radius=round(big * TILE_RADIUS),
        fill=BRAND_BLUE,
    )
    g = glyph.resize(
        (round(big * glyph_ratio), round(big * glyph_ratio * glyph.height / glyph.width)),
        Image.LANCZOS,
    )
    im.alpha_composite(g, ((big - g.width) // 2, (big - g.height) // 2))
    return im.resize((size, size), Image.LANCZOS)


def bare(glyph: Image.Image, size: int, glyph_ratio: float) -> Image.Image:
    """无底色裸字形（中/英态）：品牌蓝原字形按最大边等比缩放居中，透明背景。"""
    big = size * SS
    scale = big * glyph_ratio / max(glyph.size)
    g = recolor(glyph, BRAND_BLUE).resize(
        (round(glyph.width * scale), round(glyph.height * scale)), Image.LANCZOS
    )
    im = Image.new("RGBA", (big, big), (0, 0, 0, 0))
    im.alpha_composite(g, ((big - g.width) // 2, (big - g.height) // 2))
    return im.resize((size, size), Image.LANCZOS)


def write_ico(path: Path, images: list[Image.Image]) -> None:
    """经典 32bpp DIB 条目 ICO（BITMAPINFOHEADER + 自底向上 BGRA XOR + 全零 AND）。"""
    entries: list[tuple[int, int, bytes]] = []
    for im in images:
        s = im.size[0]
        px = im.convert("RGBA").load()
        xor = bytearray(s * s * 4)
        for y in range(s):
            row = s - 1 - y  # ICO 存储首行为最底行
            for x in range(s):
                r, g, b, a = px[x, y]
                i = (row * s + x) * 4
                xor[i], xor[i + 1], xor[i + 2], xor[i + 3] = b, g, r, a
        header = (
            (40).to_bytes(4, "little")
            + s.to_bytes(4, "little")
            + (s * 2).to_bytes(4, "little")  # biHeight = XOR + AND 两段
            + (1).to_bytes(2, "little")
            + (32).to_bytes(2, "little")
            + (0).to_bytes(4, "little")      # BI_RGB
            + (s * s * 4).to_bytes(4, "little")
            + (0).to_bytes(16, "little")     # biX/YPPM + biClrUsed/Important，凑满 40 字节
        )
        and_len = s * ((s + 31) // 32) * 4  # 32bpp 走 alpha，AND 掩码全零
        entries.append((s, len(header) + len(xor) + and_len, header + bytes(xor) + b"\0" * and_len))

    header_len = 6 + 16 * len(entries)
    out = bytearray(
        (0).to_bytes(2, "little")
        + (1).to_bytes(2, "little")      # type = icon
        + len(entries).to_bytes(2, "little")
    )
    offset = header_len
    for s, length, _ in entries:
        out += (
            (s if s < 256 else 0).to_bytes(1, "little")
            + (s if s < 256 else 0).to_bytes(1, "little")
            + (0).to_bytes(1, "little")
            + (0).to_bytes(1, "little")
            + (1).to_bytes(2, "little")
            + (32).to_bytes(2, "little")
            + length.to_bytes(4, "little")
            + offset.to_bytes(4, "little")
        )
        offset += length
    for _, _, data in entries:
        out += data
    path.write_bytes(out)
    print(f"OK：{path}（{len(out)} 字节，{len(entries)} 个尺寸）")


def main() -> None:
    zh = crop_alpha(ASSETS / "toolbar-icons" / "lang-cn.png")
    en = crop_alpha(ASSETS / "toolbar-icons" / "lang-en.png")
    iuv = crop_alpha(ASSETS / "main.png")

    white_iuv = recolor(iuv, WHITE)

    small_sizes = [16, 24, 32, 48]
    big_sizes = [16, 24, 32, 48, 64, 128, 256]

    write_ico(
        RES / "zh.ico",
        [bare(zh, s, GLYPH_RATIO) for s in small_sizes],
    )
    write_ico(
        RES / "en.ico",
        [bare(en, s, GLYPH_RATIO) for s in small_sizes],
    )
    write_ico(
        RES / "icon.ico",
        [tile(white_iuv, s, WORD_RATIO) for s in big_sizes],
    )

    # 预览图：深/浅双底各一排（16px 原尺寸 + 24px 3x），存 target/ 不进 git。
    preview_dir = ROOT / "target" / "tsf-icon-preview"
    preview_dir.mkdir(parents=True, exist_ok=True)
    row_h, pad = 96, 12
    sheet = Image.new("RGBA", (3 * 150 + pad, row_h * 2 + pad * 3), (0, 0, 0, 0))
    for r, bg in enumerate(((24, 24, 24, 255), (240, 240, 240, 255))):
        d = ImageDraw.Draw(sheet)
        d.rectangle([0, pad + r * (row_h + pad), sheet.width - 1, pad + r * (row_h + pad) + row_h - 1], fill=bg)
        x = pad
        for glyph, ratio, style in ((iuv, WORD_RATIO, "tile"), (zh, GLYPH_RATIO, "bare"), (en, GLYPH_RATIO, "bare")):
            if style == "tile":
                im16 = tile(recolor(glyph, WHITE), 16, ratio)
                im24 = tile(recolor(glyph, WHITE), 24, ratio)
            else:
                im16 = bare(glyph, 16, ratio)
                im24 = bare(glyph, 24, ratio)
            sheet.alpha_composite(im16, (x, pad + r * (row_h + pad) + 10))
            sheet.alpha_composite(im24.resize((72, 72), Image.NEAREST), (x + 30, pad + r * (row_h + pad) + 14))
            x += 150
    sheet.save(preview_dir / "tray-preview.png")
    print(f"预览：{preview_dir / 'tray-preview.png'}")


if __name__ == "__main__":
    main()
