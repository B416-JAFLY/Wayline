#!/usr/bin/env python3
"""Compose either direction of the README AirDrop / ESP32 / Omdrop walkthrough.

The phone and desktop panels are illustrative. The board panel is the actual
firmware renderer. This script does not start services, open ports or send files.
Requires Pillow; pass --font for a CJK font on non-Windows systems.
"""
import argparse
import csv
import json
import math
from functools import lru_cache
from pathlib import Path

from PIL import Image, ImageColor, ImageDraw, ImageFont

ROOT = Path(__file__).resolve().parents[1]
WORK = ROOT / "build/cardputer-ui/airdrop-flow"
OUTPUT = ROOT / "docs/assets/airdrop-esp32-omdrop.gif"
WIDTH, HEIGHT, SCALE = 1280, 720, 2
BG, CARD, WHITE, MUTED, BLUE = "#0b1018", "#1c1c1f", "#f5f5f7", "#a0a0a8", "#0a84ff"
STATES = ["photos", "photo", "share", "discover", "offer", "transfer", "confirm", "complete"]
DURATION_MS = 24_000
PHONE_X, PHONE_Y, PHONE_W, PHONE_H = 32, 198, 218, 460
CAPTIONS = {
    "photos": "打开 iPhone「照片」，轻点要分享的照片",
    "photo": "轻点左下角的分享按钮，打开 iOS 分享面板",
    "share": "在系统分享面板中轻点「隔空投送」",
    "discover": "在 iPhone 的 AirDrop 列表中选择 Omdrop PC",
    "offer": "请求经 ESP32 到达电脑，在 Omdrop 点击「接受」",
    "transfer": "文件通过 AWDL → ESP32 → USB 送入 Omdrop",
    "confirm": "进度已到 100%，继续等待文件校验和协议确认",
    "complete": "文件保存到「下载 / Omdrop」，发送端与板端显示完成",
}
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--font", type=Path, help="CJK TrueType/OpenType font")
parser.add_argument("--direction", choices=["iphone-to-pc", "pc-to-iphone"],
                    default="iphone-to-pc", help="transfer direction (default: iphone-to-pc)")
args = parser.parse_args()
PC_TO_IPHONE = args.direction == "pc-to-iphone"
DESKTOP_X, BOARD_X = (24, 460) if PC_TO_IPHONE else (876, 320)
if PC_TO_IPHONE:
    WORK = ROOT / "build/cardputer-ui/pc-airdrop-flow"
    OUTPUT = ROOT / "docs/assets/omdrop-esp32-airdrop.gif"
    PHONE_X = 1030
    STATES = ["home", "picker", "discover", "prepare", "offer", "transfer", "confirm", "complete"]
    CAPTIONS = {
        "home": "在 Omdrop 点击「选择文件」；iPhone 保持唤醒并开启隔空投送接收",
        "picker": "在 Windows 文件选择器中选中照片，点击「打开」",
        "discover": "在附近设备中选中 iPhone，然后点击「发送」",
        "prepare": "Omdrop 正在整理文件，并通过 ESP32 联系接收设备",
        "offer": "iPhone 显示隔空投送请求，轻点「接受」后开始传输",
        "transfer": "照片从 Omdrop 经 USB → ESP32 → AWDL 发送到 iPhone",
        "confirm": "上传已到 100%，Omdrop 继续等待 iPhone 的接收确认",
        "complete": "iPhone 查看收到的照片，Omdrop 与 ESP32 显示发送完成",
    }
candidates = [args.font, Path("C:/Windows/Fonts/msyh.ttc"),
              Path("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
              Path("/System/Library/Fonts/PingFang.ttc")]
FONT = next((path for path in candidates if path and path.is_file()), None)
if FONT is None:
    raise SystemExit("Pass --font with a CJK font file")
logo = Image.open(ROOT / "apps/desktop/assets/icon.png").convert("RGBA")


@lru_cache(maxsize=16)
def font(size):
    return ImageFont.truetype(str(FONT), round(size * SCALE))


def text(draw, xy, value, size=14, color=WHITE, anchor="lt"):
    draw.text(tuple(round(v * SCALE) for v in xy), value, font=font(size), fill=color, anchor=anchor)


def rounded(draw, bounds, radius, color, outline=None, width=1):
    draw.rounded_rectangle(tuple(round(v * SCALE) for v in bounds), round(radius * SCALE),
                           fill=color, outline=outline, width=round(width * SCALE))


def line(draw, points, color, width=1):
    draw.line([(round(x * SCALE), round(y * SCALE)) for x, y in points], fill=color,
              width=round(width * SCALE), joint="curve")


def circle(draw, cx, cy, radius, color, outline=None, width=1):
    bounds = ((cx-radius)*SCALE, (cy-radius)*SCALE, (cx+radius)*SCALE, (cy+radius)*SCALE)
    draw.ellipse(bounds, fill=color, outline=outline, width=round(width*SCALE))


def mark(image, x, y, size):
    image.alpha_composite(logo.resize((round(size*SCALE), round(size*SCALE)), Image.Resampling.LANCZOS),
                          (round(x*SCALE), round(y*SCALE)))


def gear(draw, cx, cy):
    points = []
    for tooth in range(8):
        angle = tooth * math.tau / 8
        for offset, radius in [(-0.16, 5.1), (-0.16, 6.6), (0.16, 6.6), (0.16, 5.1)]:
            points.append((cx+math.cos(angle+offset)*radius, cy+math.sin(angle+offset)*radius))
    line(draw, points + points[:1], MUTED, 1.25)
    circle(draw, cx, cy, 2.2, None, MUTED, 1.25)


def button(draw, x, y, label, primary=True):
    rounded(draw, (x, y, x+312, y+42), 13, BLUE if primary else "#303037")
    text(draw, (x+156, y+11), label, 13, WHITE, "mt")


def polygon(draw, points, color):
    draw.polygon([(round(x*SCALE), round(y*SCALE)) for x, y in points], fill=color)


def arc(draw, cx, cy, radius, start, end, color, width=1):
    draw.arc(tuple(round(v*SCALE) for v in (cx-radius, cy-radius, cx+radius, cy+radius)),
             start, end, fill=color, width=round(width*SCALE))


def share_icon(draw, cx, cy, color=BLUE):
    line(draw, [(cx-7, cy-2), (cx-7, cy+8), (cx+7, cy+8), (cx+7, cy-2)], color, 1.4)
    line(draw, [(cx, cy+2), (cx, cy-10)], color, 1.4)
    line(draw, [(cx-4, cy-6), (cx, cy-10), (cx+4, cy-6)], color, 1.4)


def airdrop_icon(draw, cx, cy, color=BLUE, size=1):
    for radius in (5, 9, 13):
        arc(draw, cx, cy, radius*size, 135, 405, color, 1.5*size)
    polygon(draw, [(cx, cy+3*size), (cx-5*size, cy+12*size), (cx+5*size, cy+12*size)], color)


@lru_cache(maxsize=12)
def sample_photo(variant, width, height):
    """Procedural sample scenery, shared by the library, photo viewer and share sheet."""
    image = Image.new("RGBA", (round(width*SCALE), round(height*SCALE)))
    draw = ImageDraw.Draw(image)
    skies = [(126, 190, 211), (207, 169, 139), (138, 176, 188), (151, 183, 164)]
    sky = skies[variant % len(skies)]
    for y in range(image.height):
        mix = y/image.height
        color = tuple(round(channel*(1-mix)+235*mix) for channel in sky)
        draw.line((0, y, image.width, y), fill=color)
    circle(draw, width*.73, height*.2, width*.055, "#fff3d2")
    polygon(draw, [(0, height*.59), (width*.25, height*.22), (width*.43, height*.49),
                   (width*.72, height*.3), (width, height*.64), (width, height), (0, height)], "#7197a1")
    polygon(draw, [(width*.13, height*.39), (width*.25, height*.22), (width*.35, height*.37),
                   (width*.28, height*.34), (width*.24, height*.4), (width*.21, height*.36)], "#edf3ef")
    polygon(draw, [(0, height*.7), (width*.2, height*.49), (width*.49, height*.73),
                   (width*.8, height*.5), (width, height*.68), (width, height), (0, height)], "#426e78")
    polygon(draw, [(0, height*.73), (width, height*.67), (width, height), (0, height)], "#83b9c0")
    for index in range(7):
        y = height*(.77+index*.027)
        line(draw, [(width*(.18+index%3*.12), y), (width*(.7+index%2*.1), y)], "#c1d9d6", .5)
    polygon(draw, [(0, height*.68), (width*.19, height*.79), (width*.28, height), (0, height)], "#294f47")
    polygon(draw, [(width, height*.71), (width*.89, height*.79), (width*.8, height), (width, height)], "#375c4b")
    return image


def paste_photo(image, bounds, variant=0):
    left, top, right, bottom = bounds
    image.alpha_composite(sample_photo(variant, right-left, bottom-top),
                          (round(left*SCALE), round(top*SCALE)))


def phone_status_bar(draw):
    text(draw, (12, 14), "9:41", 10, "#131316")
    rounded(draw, (82, 8, 133, 23), 8, "#08090b")
    for index in range(4):
        rounded(draw, (160+index*3, 21-index*2, 162+index*3, 23), .5, "#131316")
    arc(draw, 180, 22, 6, 225, 315, "#131316", 1.2)
    arc(draw, 180, 22, 3.5, 225, 315, "#131316", 1.2)
    circle(draw, 180, 21, .8, "#131316")
    rounded(draw, (190, 14, 206, 22), 2, None, "#77777d", .7)
    rounded(draw, (191.5, 15.5, 203, 20.5), .7, "#131316")
    line(draw, [(207.5, 17), (207.5, 19)], "#77777d", 1)


def photo_view(image):
    draw = ImageDraw.Draw(image)
    line(draw, [(17, 51), (10, 58), (17, 65)], BLUE, 1.7)
    text(draw, (109, 48), "10月17日", 12, "#151518", "mt")
    text(draw, (109, 66), "09:41", 8, "#717177", "mt")
    circle(draw, 199, 58, 10, "#f0f0f4")
    for x in (195, 199, 203):
        circle(draw, x, 58, .9, BLUE)
    paste_photo(image, (0, 130, 218, 327))
    for index in range(7):
        paste_photo(image, (6+index*30, 376, 33+index*30, 406), index % 4)
    rounded(draw, (95, 372, 123, 409), 3, None, "#ffffff", 2)
    share_icon(draw, 24, 426)
    line(draw, [(68, 423), (71, 419), (75, 419), (79, 423), (83, 419), (87, 419),
                (90, 423), (90, 427), (79, 436), (68, 427), (68, 423)], BLUE, 1.2)
    circle(draw, 133, 426, 8, None, BLUE, 1.2)
    text(draw, (133, 420), "i", 10, BLUE, "mt")
    line(draw, [(187, 421), (189, 435), (199, 435), (201, 421)], BLUE, 1.2)
    line(draw, [(185, 420), (203, 420)], BLUE, 1.2)
    line(draw, [(191, 417), (197, 417)], BLUE, 1.2)


@lru_cache(maxsize=len(STATES))
def phone_content(state):
    """A simulated iPhone Photos -> system share sheet -> AirDrop workflow."""
    image = Image.new("RGBA", (PHONE_W*SCALE, PHONE_H*SCALE), "#ffffff")
    draw = ImageDraw.Draw(image)
    if state == "photos":
        text(draw, (12, 52), "图库", 23, "#151518")
        rounded(draw, (166, 52, 206, 76), 12, "#f1f1f5")
        text(draw, (186, 58), "选择", 10, "#202024", "mt")
        text(draw, (12, 94), "10月17日", 13, "#151518")
        text(draw, (12, 115), "上海", 9, "#74747a")
        for index in range(12):
            col, row = index % 3, index // 3
            paste_photo(image, (col*73, 136+row*68, col*73+71, 202+row*68), index % 4)
        rounded(draw, (0, 415, 218, 460), 0, "#fafafd")
        for x, label in [(28, "图库"), (82, "为你"), (137, "相簿"), (191, "搜索")]:
            color = BLUE if label == "图库" else "#8a8a90"
            if label == "图库":
                for index in range(6):
                    angle = index*math.tau/6
                    circle(draw, x+math.cos(angle)*4, 427+math.sin(angle)*4, 2.5, color)
            elif label == "搜索":
                circle(draw, x-1, 426, 4.5, None, color, 1.2)
                line(draw, [(x+3, 430), (x+6, 433)], color, 1.2)
            elif label == "相簿":
                rounded(draw, (x-6, 422, x+6, 432), 1.5, None, color, 1)
                line(draw, [(x-3, 420), (x+6, 420)], color, 1)
            else:
                polygon(draw, [(x, 420), (x+2, 424), (x+6, 426), (x+2, 428),
                               (x, 434), (x-2, 428), (x-6, 426), (x-2, 424)], color)
            text(draw, (x, 438), label, 7, color, "mt")
    else:
        photo_view(image)
        if state != "photo":
            image.alpha_composite(Image.new("RGBA", image.size, (0, 0, 0, 85)))
            draw = ImageDraw.Draw(image)
            if state == "share":
                rounded(draw, (0, 109, 218, 475), 15, "#f2f2f7")
                rounded(draw, (93, 116, 125, 119), 2, "#c4c4cb")
                paste_photo(image, (12, 134, 48, 173))
                text(draw, (58, 137), "1张照片", 12, "#161619")
                text(draw, (58, 157), "选项  ›", 9, BLUE)
                circle(draw, 198, 145, 9, "#e1e1e7")
                line(draw, [(195, 142), (201, 148)], "#85858b", 1.3)
                line(draw, [(195, 148), (201, 142)], "#85858b", 1.3)
                for x, label, color in [(32, "隔空投送", "#ffffff"), (84, "信息", "#34c759"),
                                         (136, "邮件", "#1684ff"), (188, "备忘录", "#ffffff")]:
                    rounded(draw, (x-20, 196, x+20, 236), 9, color)
                    if label == "隔空投送":
                        airdrop_icon(draw, x, 215)
                    elif label == "信息":
                        rounded(draw, (x-12, 205, x+12, 222), 8, "#ffffff")
                        polygon(draw, [(x-8, 219), (x-10, 226), (x-2, 222)], "#ffffff")
                    elif label == "邮件":
                        rounded(draw, (x-13, 207, x+13, 225), 2, "#ffffff")
                        line(draw, [(x-12, 209), (x, 218), (x+12, 209)], "#1684ff", 1.2)
                    else:
                        rounded(draw, (x-20, 196, x+20, 207), 4, "#ffcc35")
                        for y in (214, 220, 226):
                            line(draw, [(x-12, y), (x+12, y)], "#d5d5db", .7)
                    text(draw, (x, 243), label, 8, "#242428", "mt")
                for index, label in enumerate(["拷贝照片", "添加到共享相簿", "存储到“文件”"]):
                    y = 273+index*47
                    rounded(draw, (9, y, 209, y+45), 7, "#ffffff")
                    text(draw, (21, y+14), label, 12, "#242428")
                    if index == 0:
                        rounded(draw, (185, y+13, 196, y+27), 1, None, "#45454b", 1)
                        line(draw, [(182, y+24), (182, y+10), (192, y+10)], "#45454b", 1)
                    else:
                        line(draw, [(191, y+14), (196, y+20), (191, y+26)], "#85858b", 1)
            else:
                rounded(draw, (0, 48, 218, 475), 15, "#f8f8fb")
                text(draw, (109, 66), "隔空投送", 14, "#17171b", "mt")
                text(draw, (203, 69), "完成" if state == "complete" else "取消", 10, BLUE, "rt")
                circle(draw, 109, 133, 25, BLUE)
                airdrop_icon(draw, 109, 131, "#ffffff", 1.2)
                text(draw, (109, 177), "与附近的人即时共享。", 10, "#242428", "mt")
                text(draw, (109, 196), "轻点下方的设备即可分享照片", 9, "#7d7d86", "mt")
                line(draw, [(12, 224), (206, 224)], "#dfdfe6", .7)
                text(draw, (16, 238), "用户", 11, "#242428")
                circle(draw, 60, 290, 27, "#e8e8ee")
                rounded(draw, (43, 278, 77, 301), 3, "#a6adb7")
                rounded(draw, (45, 280, 75, 298), 1, "#d5e5ed")
                polygon(draw, [(43, 301), (39, 306), (81, 306), (77, 301)], "#949da9")
                line(draw, [(55, 303), (65, 303)], "#dce0e5", 1)
                text(draw, (60, 329), "Omdrop PC", 10, "#1b1b20", "mt")
                status = {"discover": "", "offer": "等待中…", "transfer": "正在发送…",
                          "confirm": "正在发送…", "complete": "已发送"}[state]
                text(draw, (60, 347), status, 9, BLUE, "mt")
    phone_status_bar(ImageDraw.Draw(image))
    return image


def tap(draw, x, y, elapsed, start, end):
    if start <= elapsed < end:
        progress = (elapsed-start)/(end-start)
        radius = 12+8*math.sin(progress*math.pi)
        circle(draw, x, y, radius, None, "#78bfff", 2)
        circle(draw, x, y, 4, "#1684ff", "#ffffff", 1)


@lru_cache(maxsize=len(STATES))
def receiving_phone_content(state):
    image = Image.new("RGBA", (PHONE_W*SCALE, PHONE_H*SCALE), "#f2f2f7")
    draw = ImageDraw.Draw(image)
    if state == "complete":
        image.paste("#ffffff", (0, 0, *image.size))
        photo_view(image)
    else:
        line(draw, [(16, 51), (9, 58), (16, 65)], BLUE, 1.5)
        text(draw, (23, 52), "通用", 11, BLUE)
        text(draw, (122, 51), "隔空投送", 13, "#17171b", "mt")
        rounded(draw, (12, 101, 206, 235), 10, "#ffffff")
        for index, label in enumerate(["接收关闭", "仅限联系人", "所有人（10分钟）"]):
            y = 117+index*44
            text(draw, (24, y), label, 11, "#242428")
            if index < 2:
                line(draw, [(24, y+29), (206, y+29)], "#e1e1e7", .6)
        line(draw, [(185, 210), (189, 214), (196, 206)], BLUE, 1.6)
        text(draw, (24, 251), "可在10分钟内被附近所有人发现。", 9, "#787880")
        text(draw, (24, 270), "收到内容时，你可以接受或拒绝。", 9, "#787880")
        if state in ("offer", "transfer", "confirm"):
            image.alpha_composite(Image.new("RGBA", image.size, (0, 0, 0, 90)))
            draw = ImageDraw.Draw(image)
            rounded(draw, (14, 112, 204, 354), 14, "#f6f6f9")
            text(draw, (109, 131), "隔空投送", 16, "#17171b", "mt")
            if state == "offer":
                text(draw, (109, 160), "“Omdrop PC”想要共享", 10, "#242428", "mt")
                text(draw, (109, 179), "一张照片。", 10, "#242428", "mt")
            else:
                text(draw, (109, 160), "正在接收来自", 10, "#242428", "mt")
                text(draw, (109, 179), "Omdrop PC 的照片", 10, "#242428", "mt")
            # The sender's Ask payload has file metadata but no thumbnail.
            rounded(draw, (90, 211, 128, 259), 4, "#ffffff", "#d1d1db", .8)
            polygon(draw, [(117, 211), (128, 222), (117, 222)], "#d9eaff")
            text(draw, (109, 234), "JPG", 10, BLUE, "mt")
            text(draw, (109, 275), "IMG_4821.jpg", 9, "#7d7d86", "mt")
            line(draw, [(14, 312), (204, 312)], "#d5d5dc", .7)
            if state == "offer":
                line(draw, [(109, 312), (109, 354)], "#d5d5dc", .7)
                text(draw, (61, 326), "拒绝", 12, BLUE, "mt")
                text(draw, (157, 326), "接受", 12, BLUE, "mt")
            else:
                text(draw, (109, 326), "取消", 12, BLUE, "mt")
    phone_status_bar(ImageDraw.Draw(image))
    return image


def place_phone(image, phone):
    rounded(ImageDraw.Draw(phone), (76, 449, 142, 453), 2, "#17171b")
    mask = Image.new("L", phone.size)
    rounded(ImageDraw.Draw(mask), (0, 0, PHONE_W, PHONE_H), 25, 255)
    image.paste(phone, (PHONE_X*SCALE, PHONE_Y*SCALE), mask)


def receiving_phone_panel(image, state, elapsed, percent):
    phone = receiving_phone_content(state).copy()
    if state == "offer" and elapsed < 8300:
        phone = Image.blend(receiving_phone_content("prepare"), phone, (elapsed-8000)/300)
    draw = ImageDraw.Draw(phone)
    if state == "transfer":
        arc(draw, 109, 236, 33, -90, -90+3.6*max(percent, 1), BLUE, 2)
    elif state == "confirm":
        angle = elapsed/8 % 360
        arc(draw, 109, 236, 33, angle, angle+85, BLUE, 2)
    tap(draw, 157, 333, elapsed, 11_200, 12_000)
    place_phone(image, phone)


def phone_panel(image, state, elapsed, percent):
    phone = phone_content(state).copy()
    # System sheets rise from the lower edge of the phone display.
    if state in ("share", "discover"):
        start, top, previous = (4000, 109, "photo") if state == "share" else (6000, 48, "share")
        progress = min(1, (elapsed-start)/350)
        if progress < 1:
            eased = 1-(1-progress)**3
            sheet = phone.crop((0, top*SCALE, PHONE_W*SCALE, PHONE_H*SCALE))
            phone = phone_content(previous).copy()
            phone.alpha_composite(Image.new("RGBA", phone.size, (0, 0, 0, round(40*eased))))
            phone.alpha_composite(sheet, (0, round((top+(PHONE_H-top)*(1-eased))*SCALE)))
    draw = ImageDraw.Draw(phone)
    if state == "transfer":
        arc(draw, 60, 290, 31, -90, -90+3.6*max(percent, 1), BLUE, 2)
    elif state in ("offer", "confirm"):
        angle = elapsed/8 % 360
        arc(draw, 60, 290, 31, angle, angle+85, BLUE, 2)
    elif state == "complete":
        circle(draw, 81, 309, 8, BLUE)
        line(draw, [(77, 309), (80, 312), (85, 306)], "#ffffff", 1.4)
    for x, y, start, end in [(35, 170, 1150, 2000), (24, 426, 3200, 4000),
                             (32, 216, 5200, 6000), (60, 290, 7100, 8000)]:
        tap(draw, x, y, elapsed, start, end)
    place_phone(image, phone)


def sending_desktop(image, state):
    draw = ImageDraw.Draw(image)
    x, y = DESKTOP_X, 190
    if state == "discover":
        rounded(draw, (x+22, y+84, x+334, y+174), 15, "#27272b")
        paste_photo(image, (x+34, y+98, x+101, y+155))
        text(draw, (x+114, y+99), "IMG_4821.jpg", 12)
        text(draw, (x+114, y+125), "1.0 MB", 10, MUTED)
        text(draw, (x+218, y+147), "打开文件", 9, BLUE)
        text(draw, (x+277, y+147), "更换文件", 9, BLUE)
        text(draw, (x+22, y+202), "发送到附近设备", 11, MUTED)
        text(draw, (x+334, y+202), "搜索其他设备…", 10, BLUE, "rt")
        return
    complete = state == "complete"
    circle(draw, x+178, y+128, 32, "#203b2b" if complete else "#233249")
    if complete:
        line(draw, [(x+163, y+129), (x+174, y+140), (x+195, y+117)], "#30d158", 3)
    else:
        line(draw, [(x+178, y+145), (x+178, y+111)], BLUE, 3)
        line(draw, [(x+164, y+125), (x+178, y+111), (x+192, y+125)], BLUE, 3)
    title = {"prepare": "正在准备", "offer": "等待对方接受", "transfer": "正在发送",
             "confirm": "等待接收确认", "complete": "已送达"}[state]
    subtitle = {"prepare": "正在整理文件", "offer": "请在接收设备上轻点「接受」",
                "transfer": "请保持设备靠近", "confirm": "文件已上传，正在等待对方确认",
                "complete": "文件已被接收设备确认"}[state]
    text(draw, (x+178, y+183), title, 22, WHITE, "mt")
    text(draw, (x+178, y+221), "iPhone", 12, MUTED, "mt")
    text(draw, (x+178, y+251), subtitle, 11, MUTED, "mt")
    button(draw, x+22, y+351, "完成" if complete else "取消发送", complete)


def file_picker(image):
    draw = ImageDraw.Draw(image)
    x, y = DESKTOP_X+9, 269
    rounded(draw, (x, y, x+338, y+312), 9, "#fafafa", "#787878")
    rounded(draw, (x, y, x+338, y+30), 9, "#f1f1f1")
    text(draw, (x+12, y+10), "打开", 11, "#252529")
    text(draw, (x+318, y+7), "×", 14, "#55555b", "mt")
    rounded(draw, (x+12, y+40, x+326, y+68), 4, "#ffffff", "#dedee4")
    text(draw, (x+23, y+49), "此电脑  ›  图片", 10, "#4b4b52")
    rounded(draw, (x+1, y+77, x+76, y+246), 0, "#f0f0f2")
    for index, label in enumerate(["桌面", "下载", "文档", "图片"]):
        yy = y+94+index*30
        if label == "图片":
            rounded(draw, (x+5, yy-6, x+72, yy+18), 3, "#d9eafa")
        text(draw, (x+19, yy), label, 10, "#3b3b42")
    rounded(draw, (x+88, y+87, x+227, y+224), 5, "#dbeeff", "#8abbea")
    paste_photo(image, (x+101, y+100, x+214, y+182))
    text(draw, (x+157, y+199), "IMG_4821.jpg", 10, "#28282d", "mt")
    text(draw, (x+14, y+254), "文件名：", 9, "#414149")
    rounded(draw, (x+72, y+246, x+326, y+272), 3, "#ffffff", "#a4a4ac")
    text(draw, (x+80, y+254), "IMG_4821.jpg", 10, "#222228")
    for left, label in [(173, "打开"), (253, "取消")]:
        rounded(draw, (x+left, y+282, x+left+72, y+304), 3,
                "#e1effc" if label == "打开" else "#f4f4f4", "#9aa7b3")
        text(draw, (x+left+36, y+288), label, 10, "#25252b", "mt")


def mouse_click(draw, elapsed, start, end, origin, target):
    if not start <= elapsed < end:
        return
    progress = min(1, (elapsed-start)/(end-start-250))
    eased = progress*progress*(3-2*progress)
    x, y = (origin[i]+(target[i]-origin[i])*eased for i in range(2))
    if elapsed >= end-250:
        circle(draw, x, y, 14, None, "#78bfff", 2)
    cursor = [(x, y), (x+3, y+24), (x+10, y+17), (x+16, y+28), (x+21, y+25), (x+15, y+15), (x+25, y+14)]
    draw.polygon([(round(a*SCALE), round(b*SCALE)) for a, b in cursor],
                 fill="#ffffff", outline="#111827", width=SCALE)


def sending_desktop_overlay(image, state, elapsed):
    draw = ImageDraw.Draw(image)
    x, y = DESKTOP_X, 190
    if state == "picker":
        file_picker(image)
    elif state == "discover":
        selected = elapsed >= 5500
        rounded(draw, (x+22, y+229, x+334, y+294), 15,
                "#173353" if selected else "#27272b", BLUE if selected else "#3c3c43")
        rounded(draw, (x+39, y+244, x+59, y+277), 4, None, BLUE if selected else MUTED, 1.5)
        line(draw, [(x+45, y+272), (x+53, y+272)], MUTED, 1.4)
        text(draw, (x+76, y+244), "iPhone", 13)
        text(draw, (x+76, y+267), "AirDrop · iPhone-demo", 9, MUTED)
        if selected:
            line(draw, [(x+304, y+258), (x+309, y+263), (x+317, y+253)], BLUE, 2)
        button(draw, x+22, y+362, "发送" if selected else "选择一台设备", selected)
    elif state == "transfer":
        rounded(draw, (x+22, y+293, x+334, y+298), 2.5, "#3a3a40")
        fraction = (elapsed-12_000)/7000
        if fraction:
            rounded(draw, (x+22, y+293, x+22+312*fraction, y+298), 2.5, BLUE)
        done = 1_048_576*(elapsed-12_000)//7000
        size = f"{done} B" if done < 1024 else f"{done/1024:.1f} KB"
        text(draw, (x+178, y+312), f"{size} / 1.0 MB · {fraction:.0%}", 11, MUTED, "mt")
    for start, end, origin, target in [
        (900, 2000, (x+315, y+423), (x+178, y+381)),
        (2850, 4000, (x+144, y+214), (x+218, 562)),
        (4500, 5500, (x+290, y+192), (x+168, y+258)),
        (6000, 7000, (x+168, y+258), (x+178, y+381)),
    ]:
        mouse_click(draw, elapsed, start, end, origin, target)


@lru_cache(maxsize=len(STATES))
def base(state):
    image = Image.new("RGBA", (WIDTH*SCALE, HEIGHT*SCALE), BG)
    draw = ImageDraw.Draw(image)
    title = "Omdrop → ESP32 → AirDrop" if PC_TO_IPHONE else "AirDrop → ESP32 → Omdrop"
    subtitle = ("从 Windows 到 iPhone，在手机确认接收后传输照片" if PC_TO_IPHONE
                else "从 iPhone 到 Windows，文件经无线桥接后保存到电脑")
    text(draw, (24, 24), title, 30)
    text(draw, (24, 67), subtitle, 15, "#a5b4c8")
    rounded(draw, (1022, 30, 1254, 61), 15, "#1c293a")
    text(draw, (1138, 37), "流程示意 · 示例数据", 13, "#c9d9ed", "mt")
    active = {"offer": 1, "transfer": 2, "confirm": 2, "complete": 3}.get(state, 0)
    steps = (["选文件 → 选 iPhone → 发送", "iPhone 确认接受", "传输并等待确认", "完成并查看照片"]
             if PC_TO_IPHONE else ["照片 → 分享 → AirDrop", "桌面确认接受", "传输并等待确认", "完成并保存"])
    for index, label in enumerate(steps):
        x = 24 + index*312
        rounded(draw, (x, 105, x+294, 141), 10, "#163251" if index == active else "#141c29")
        text(draw, (x+16, 114), f"{index+1:02}  {label}", 14,
             "#75c9ff" if index == active else "#8d9eb4")
    for x, label in [(PHONE_X-8, "AirDrop · iPhone"), (BOARD_X, "ESP32 · Cardputer Adv"),
                     (DESKTOP_X, "Omdrop · Windows")]:
        text(draw, (x, 158), label, 17)

    rounded(draw, (PHONE_X-8, 190, PHONE_X+PHONE_W+8, 666), 32, "#202936", "#586477")
    line(draw, [(PHONE_X-10, 281), (PHONE_X-10, 308)], "#586477", 2)
    line(draw, [(PHONE_X-10, 321), (PHONE_X-10, 348)], "#586477", 2)
    line(draw, [(PHONE_X+PHONE_W+10, 292), (PHONE_X+PHONE_W+10, 337)], "#586477", 2)

    # Exact native panel is pasted here after supersampling the surrounding diagram.
    rounded(draw, (BOARD_X-12, 270, BOARD_X+492, 564), 18, "#182231", "#354357", 2)
    text(draw, (BOARD_X+240, 586), "无线桥接 · USB 连接电脑", 15, "#a5b4c8", "mt")
    text(draw, (BOARD_X+240, 612), "板端只显示状态，不代替用户接受文件", 12, "#7f91aa", "mt")

    # Desktop illustration follows ui.rs: incoming approval, then the home card
    # with receiver_notice. Do not invent a receiving progress bar or auto-accept.
    x, y = DESKTOP_X, 190
    rounded(draw, (x, y, x+356, y+476), 28, CARD, "#414146")
    mark(image, x+22, y+23, 25)
    text(draw, (x+56, y+28), "Omdrop", 17)
    circle(draw, x+279, y+37, 14, "#27272b")
    gear(draw, x+279, y+37)
    circle(draw, x+315, y+37, 14, "#27272b")
    text(draw, (x+315, y+24), "×", 21, MUTED, "mt")
    if PC_TO_IPHONE and state not in ("home", "picker"):
        sending_desktop(image, state)
    elif state == "offer":
        text(draw, (x+22, y+111), "接收文件", 24)
        text(draw, (x+22, y+158), "iPhone 想向你发送 1 个文件", 13)
        text(draw, (x+22, y+192), "IMG_4821.jpg", 12, MUTED)
        text(draw, (x+22, y+227), "接受后保存到「下载 / Omdrop」", 10, MUTED)
        button(draw, x+22, y+253, "接受")
        button(draw, x+22, y+311, "拒绝", False)
    else:
        text(draw, (x+178, y+258), "轻轻一拖，即刻分享", 23, WHITE, "mt")
        text(draw, (x+178, y+304), "将文件拖到这里", 12, MUTED, "mt")
        text(draw, (x+178, y+324), "发送到附近的 iPhone 或 Mac", 12, MUTED, "mt")
        button(draw, x+22, y+362, "选择文件")
        if state in ("transfer", "confirm"):
            text(draw, (x+22, y+420), "正在接收来自 iPhone 的文件…", 10, MUTED)
        elif state == "complete":
            text(draw, (x+22, y+414), "已接收来自 iPhone 的 1 个文件，", 10, MUTED)
            text(draw, (x+22, y+430), "已保存到下载文件夹中的 Omdrop", 10, MUTED)
    circle(draw, x+25, y+454, 2.5, "#30d158")
    text(draw, (x+34, y+449), "已连接", 10, MUTED)
    text(draw, (x+334, y+449), "Ctrl+O  选择文件", 10, MUTED, "rt")
    rounded(draw, (24, 678, 1256, 711), 9, "#162336")
    text(draw, (640, 686), CAPTIONS[state], 15, "#cfe7ff", "mt")
    return image


POINTS = []
for row in range(19):
    latitude = (row + 0.5) * math.pi / 19
    radius, y = math.sin(latitude), math.cos(latitude)
    count = round(38 * radius)
    for column in range(count):
        angle = (column + (0.5 if row % 2 else 0)) * math.tau / count
        POINTS.append((radius * math.cos(angle), y, radius * math.sin(angle)))


def desktop_sphere(draw, elapsed):
    yaw = 0.35 + elapsed / 20_000 * math.tau
    scale = 1 + 0.10 * math.sin(elapsed / 3800 * math.tau)
    dots = []
    for px, py, pz in POINTS:
        x = px*math.cos(yaw) + pz*math.sin(yaw)
        z = pz*math.cos(yaw) - px*math.sin(yaw)
        y, z = py*math.cos(0.28) - z*math.sin(0.28), py*math.sin(0.28) + z*math.cos(0.28)
        front = (z+1)/2
        perspective = 3.8/(3.8-z*scale)
        alpha = 0.19 + 0.81*front**1.6
        color = tuple(round(base + (channel*255-base)*alpha)
                      for base, channel in zip((28, 28, 31), (0.12+0.43*front, 0.42+0.35*front, 1)))
        diameter = (0.020+0.012*front)*perspective*scale*57
        dots.append((z, DESKTOP_X+178+x*scale*perspective*57, 354+y*scale*perspective*57, diameter/2, color))
    for _, x, y, radius, color in sorted(dots):
        circle(draw, x, y, radius, color)


def render(row):
    state, elapsed, percent = row["state"], int(row["elapsed_ms"]), int(row["percent"])
    image = base(state).copy()
    if PC_TO_IPHONE:
        receiving_phone_panel(image, state, elapsed, percent)
    else:
        phone_panel(image, state, elapsed, percent)
    draw = ImageDraw.Draw(image)
    if (PC_TO_IPHONE and state in ("home", "picker")) or (not PC_TO_IPHONE and state != "offer"):
        desktop_sphere(draw, elapsed)
    if PC_TO_IPHONE:
        sending_desktop_overlay(image, state, elapsed)
    color = {"offer": "#e9b66b", "transfer": "#65c7ff", "confirm": "#65c7ff",
             "complete": "#39c996"}.get(state, "#43536a")
    connections = ([(390, 438, "USB"), (962, 1012, "AWDL")] if PC_TO_IPHONE
                   else [(268, 304, "AWDL"), (824, 864, "USB")])
    for left, right, label in connections:
        line(draw, [(left, 422), (right, 422)], color, 2)
        line(draw, [(right-7, 416), (right, 422), (right-7, 428)], color, 2)
        text(draw, ((left+right)/2, 389), label, 12, "#a5b4c8", "mt")
        if state in ("offer", "transfer", "confirm"):
            position = (elapsed % 900) / 900
            circle(draw, left+(right-left-9)*position, 422, 3.5, color)
    if not PC_TO_IPHONE and state == "offer" and elapsed >= 10_500:
        progress = min(1, (elapsed-10_500)/900)
        eased = progress*progress*(3-2*progress)
        x, y = 1210-154*eased, 621-163*eased
        if elapsed >= 11_700:
            circle(draw, 1056, 458, 14, None, "#caeaff", 2)
        cursor = [(x, y), (x+3, y+24), (x+10, y+17), (x+16, y+28), (x+21, y+25), (x+15, y+15), (x+25, y+14)]
        draw.polygon([(round(a*SCALE), round(b*SCALE)) for a, b in cursor], fill="#ffffff", outline="#111827", width=SCALE)
    result = image.convert("RGB").resize((WIDTH, HEIGHT), Image.Resampling.LANCZOS)
    with Image.open(WORK / row["file"]) as board:
        result.paste(board.resize((480, 270), Image.Resampling.NEAREST), (BOARD_X, 282))
    return result


def main():
    with (WORK / "timeline.tsv").open(encoding="utf-8") as stream:
        timeline = list(csv.DictReader(stream, delimiter="\t"))
    assert [int(row["elapsed_ms"]) for row in timeline] == list(range(0, DURATION_MS, 50))
    assert list(dict.fromkeys(row["state"] for row in timeline)) == STATES
    assert all(int(row["direction"]) == (0 if PC_TO_IPHONE else 1) for row in timeline)
    assert all(int(row["percent"]) == 0 for row in timeline
               if row["state"] not in ("transfer", "confirm", "complete"))
    assert all(int(row["percent"]) == 100 for row in timeline if row["state"] in ("confirm", "complete"))
    for row in timeline:
        assert Path(row["file"]).name == row["file"]
        render(row).save(WORK / f"flow-{Path(row['file']).stem}.png")
    palette_sample = Image.new("RGB", (320, 180*len(timeline[::10])))
    for index, row in enumerate(timeline[::10]):
        with Image.open(WORK / f"flow-{Path(row['file']).stem}.png") as frame:
            palette_sample.paste(frame.resize((320, 180)), (0, index*180))
    # Small iOS app icons otherwise lose their green/yellow to the dark diagram's
    # dominant colors. Reserve UI accents while sharing the other colors globally.
    accents = ["#34c759", "#ffcc35", "#1684ff", "#0a84ff", "#30d158", "#39c996",
               "#e9b66b", "#65c7ff", "#fff3d2", "#edf3ef", "#7197a1", "#426e78",
               "#83b9c0", "#c1d9d6", "#294f47", "#375c4b", "#cfa98b", "#97b7a4"]
    adaptive_colors = 256-len(accents)
    palette = palette_sample.quantize(colors=adaptive_colors, method=Image.Quantize.MEDIANCUT)
    palette.putpalette(palette.getpalette()[:adaptive_colors*3]
                       + [channel for color in accents for channel in ImageColor.getrgb(color)])
    encoded = []
    for row in timeline:
        with Image.open(WORK / f"flow-{Path(row['file']).stem}.png") as frame:
            encoded.append(frame.quantize(palette=palette, dither=Image.Dither.NONE))
    temporary = WORK / OUTPUT.name
    encoded[0].save(temporary, save_all=True, append_images=encoded[1:], duration=50,
                    loop=0, disposal=1, optimize=True)
    with Image.open(temporary) as gif:
        assert gif.size == (WIDTH, HEIGHT) and gif.info.get("loop") == 0 and gif.n_frames > 100
        duration = 0
        for index in range(gif.n_frames):
            gif.seek(index)
            duration += gif.info.get("duration", 0)
        assert duration == DURATION_MS
        frame_count = gif.n_frames
    assert temporary.stat().st_size < 8*1024*1024, "Workflow GIF exceeds the README budget"
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_bytes(temporary.read_bytes())
    summary = {"path": str(OUTPUT), "duration_ms": duration, "frames": frame_count,
               "bytes": OUTPUT.stat().st_size, "states": STATES, "direction": args.direction,
               "board": "actual firmware renderer", "phone_and_desktop": "illustrative",
               "transfer_data": "simulated"}
    (WORK / "verification.json").write_text(json.dumps(summary, indent=2), encoding="utf-8")
    print(json.dumps(summary))


if __name__ == "__main__":
    main()
