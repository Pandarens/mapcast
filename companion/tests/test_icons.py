"""Pin icons: finding an addon's file from its game path, cutting and tinting it, serving it."""
import asyncio
import os

from aiohttp.test_utils import TestClient, TestServer
from PIL import Image

from mapcast.app import Hub, create_app
from mapcast.icons import PinIcons, cut_and_tint
from mapcast.maps import MapImages
from mapcast.openmap import WorldImages


def addons_folder(root) -> str:
    folder = os.path.join(root, "AddOns", "Questie", "Icons")
    os.makedirs(folder)
    image = Image.new("RGBA", (4, 2), (0, 0, 0, 255))
    image.putpixel((0, 0), (255, 0, 0, 255))  # left half red at the top, right half white
    image.putpixel((1, 0), (255, 0, 0, 255))
    for x in (2, 3):
        image.putpixel((x, 0), (255, 255, 255, 255))
    image.save(os.path.join(folder, "Slay.TGA"))
    return os.path.join(root, "AddOns")


def test_find_addon_file_ignores_case_slashes_and_extension(tmp_path):
    icons = PinIcons(str(tmp_path / "cache"), MapImages(str(tmp_path / "cache"), None), addons_folder(str(tmp_path)))
    want = os.path.normcase(os.path.join(str(tmp_path), "AddOns", "Questie", "Icons", "Slay.TGA"))
    found = icons.find_addon_file(r"Interface\AddOns\questie\icons\slay")
    assert found and os.path.normcase(found) == want and os.path.isfile(found)  # any spelling the disk accepts
    assert os.path.normcase(icons.find_addon_file("interface/addons/Questie/Icons/slay.tga")) == want
    assert icons.find_addon_file(r"Interface\AddOns\Questie\Icons\missing.blp") is None
    assert icons.find_addon_file(r"Interface\Icons\INV_Misc_Bag_10") is None  # a game file: no addon has it


def test_cut_and_tint():
    image = Image.new("RGBA", (4, 4), (255, 255, 255, 255))
    image.putpixel((0, 0), (255, 0, 0, 255))
    left_top = cut_and_tint(image, [0, 0.5, 0, 0.5], [255, 255, 255, 255])
    assert left_top.size == (2, 2) and left_top.getpixel((0, 0)) == (255, 0, 0, 255)
    flipped = cut_and_tint(image, [0.5, 0, 0, 0.5], [255, 255, 255, 255])  # right before left: mirrored
    assert flipped.getpixel((1, 0)) == (255, 0, 0, 255)
    tinted = cut_and_tint(image, [0, 1, 0, 1], [255, 128, 0, 128])
    assert tinted.getpixel((3, 3)) == (255, 128, 0, 128)
    big = cut_and_tint(Image.new("RGBA", (256, 128)), [0, 1, 0, 1], [255] * 4)
    assert big.size == (64, 32)


PINS = {
    "sources": ["Questie", "SilverDragon"],
    "icons": [
        {"fileID": None, "path": r"Interface\AddOns\Questie\Icons\slay", "coords": [0, 0.5, 0, 0.5], "color": [255] * 4},
        {"fileID": 132320, "path": None, "coords": [0, 1, 0, 1], "color": [255] * 4},
    ],
    "pins": [{"source": 0, "u": 0.7, "v": 0.6, "icon": 0, "size": 16, "title": "Волк", "sub": None, "label": None,
              "active": False}],
}


def test_pins_reach_the_page_with_icon_urls(tmp_path):
    maps = MapImages(str(tmp_path / "cache"), None)
    icons = PinIcons(str(tmp_path / "cache"), maps, addons_folder(str(tmp_path)))
    hub = Hub(maps, WorldImages(str(tmp_path / "cache"), maps), icons)

    async def scenario():
        await hub.publish("pins", PINS)
        pins = hub.snapshot["pins"]
        assert pins["sources"] == ["Questie", "SilverDragon"] and pins["pins"] == PINS["pins"]
        url, missing = pins["icons"]
        assert missing is None  # a game texture without the game's files: the page draws a dot
        async with TestClient(TestServer(create_app(hub, "secret", "http://x"))) as client:
            reply = await client.get(url + "?t=secret")
            assert reply.status == 200
            path = icons.icon_path(url.split("/")[-1][:-4])
            with Image.open(path) as im:
                assert im.size == (2, 1) and im.convert("RGBA").getpixel((0, 0)) == (255, 0, 0, 255)
            assert (await client.get("/icon/0123456789abcdef.png?t=secret")).status == 404

    asyncio.run(scenario())
