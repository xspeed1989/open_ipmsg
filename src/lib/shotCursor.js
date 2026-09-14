/**
 * 截图「画笔」工具的光标图片 —— **生成文件，别手改**（改图形请重跑
 * `scripts/gen_shot_cursor.py`，那里有画法与尺寸取舍的完整说明）。
 *
 * 32×32 PNG，热点在笔尖 (3,3)；结尾的 `crosshair` 是兜底：图片没加载出来时退化成
 * 十字准星，而不是默认箭头。CSS 没有「笔」这个关键字，只能自带图片。
 */
export const PEN_CURSOR =
  'url("data:image/png;base64,'
  + 'iVBORw0KGgoAAAANSUhEUgAAACAAAAAgCAYAAABzenr0AAABWklEQVR42u2XoW6EQBCGT1RWk15oTvAApALZoKrRyBoEjgcg'
  + 'QVZU1OJ4gDrsCUSDICW5BNNHaE6crut0vqabIGnS3avgSzY5tf83s5NdbrNZ+eFdeVGelHvlRrlQnAkQXhSFNE0jp9NJ4EN5'
  + 'VR4V6zJUTrjv+xIEgeR5Ll3XieFZsSpB26mccCTMiqJI6rq2L8GZE0LlcwGzqqqyK8GmnDltN6FJkkgYhu4kGDgCaDuBhB8O'
  + 'BynLUna7nX0Jpp3NOXNTNeHH41HGcZQ0Te1KsBmbsjkhhFE54Uiw2rb97oxTCSo3AqxhGNxLUPlZJQgjdJVYIvGg/Pkl9RsJ'
  + 'LrRL5WwSPGq3ipXreolElmXC22Ll0VoiMU3T53a7vbb2dC+R6Pv+zfO8K6sSTDsDx5nTdip3KgFMOwPHmdN2QucS2qi9049b'
  + 'Kp5L8Nv5FzYSVE54HMd363+OlZV/zRdve42jrT2KVQAAAABJRU5ErkJggg=='
  + '") 3 3, crosshair'
