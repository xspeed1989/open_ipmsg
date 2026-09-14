// node --test scripts/ —— 截图选区几何纯函数单测（不依赖 Tauri / 浏览器）
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import {
  rectFromDrag, clampRect, canConfirm, hitTestHandle,
  resizeRect, moveRect, nudgeRect,
} from '../src/lib/shot.js'

const B = { x: 0, y: 0, w: 100, h: 80 }

test('拖拽两个方向都得到归一化矩形', () => {
  assert.deepEqual(rectFromDrag({ x: 10, y: 20 }, { x: 40, y: 60 }, B), { x: 10, y: 20, w: 30, h: 40 })
  // 反向拖（从右下往左上）结果相同
  assert.deepEqual(rectFromDrag({ x: 40, y: 60 }, { x: 10, y: 20 }, B), { x: 10, y: 20, w: 30, h: 40 })
})

test('拖出边界被夹回窗口内', () => {
  assert.deepEqual(rectFromDrag({ x: -20, y: -20 }, { x: 200, y: 200 }, B), B)
  assert.deepEqual(clampRect({ x: 90, y: 70, w: 40, h: 40 }, B), { x: 90, y: 70, w: 10, h: 10 })
})

test('最小可确认尺寸是 3×3', () => {
  assert.equal(canConfirm({ x: 0, y: 0, w: 2, h: 9 }), false)
  assert.equal(canConfirm({ x: 0, y: 0, w: 3, h: 3 }), true)
  assert.equal(canConfirm(null), false)
})

test('手柄命中：角优先于边，边优先于内部', () => {
  const r = { x: 10, y: 10, w: 50, h: 40 }
  assert.equal(hitTestHandle(r, { x: 11, y: 11 }), 'nw')
  assert.equal(hitTestHandle(r, { x: 59, y: 11 }), 'ne')
  assert.equal(hitTestHandle(r, { x: 59, y: 49 }), 'se')
  assert.equal(hitTestHandle(r, { x: 11, y: 49 }), 'sw')
  assert.equal(hitTestHandle(r, { x: 35, y: 10 }), 'n')
  assert.equal(hitTestHandle(r, { x: 60, y: 30 }), 'e')
  assert.equal(hitTestHandle(r, { x: 35, y: 30 }), 'inside')
  assert.equal(hitTestHandle(r, { x: 90, y: 30 }), 'outside')
})

test('小选区（3×3）内部仍能命中 inside，不被手柄吃光', () => {
  // 回归：容差若写成固定 tol=6 的半径，3×3 选区的任何内部点都会落到手柄上，
  // 于是小选区只能被拉伸、无法整体移动（Task 7 的 move 分支变成死代码）
  const small = { x: 10, y: 10, w: 3, h: 3 }
  assert.equal(hitTestHandle(small, { x: 11.5, y: 11.5 }), 'inside')
  assert.equal(hitTestHandle(small, { x: 10, y: 10 }), 'nw')
  assert.equal(hitTestHandle(small, { x: 11.5, y: 13 }), 's')
})

test('拖手柄只动被拖的边，且夹在窗口内', () => {
  const r = { x: 10, y: 10, w: 50, h: 40 }
  assert.deepEqual(resizeRect(r, 'se', { x: 80, y: 70 }, B), { x: 10, y: 10, w: 70, h: 60 })
  assert.deepEqual(resizeRect(r, 'nw', { x: 0, y: 0 }, B), { x: 0, y: 0, w: 60, h: 50 })
  // 拖过头：夹回边界，不允许负宽
  assert.deepEqual(resizeRect(r, 'se', { x: -50, y: -50 }, B), { x: 0, y: 0, w: 10, h: 10 })
})

test('移动与方向键微调都夹在窗口内', () => {
  const r = { x: 10, y: 10, w: 50, h: 40 }
  assert.deepEqual(moveRect(r, 5, -5, B), { x: 15, y: 5, w: 50, h: 40 })
  assert.deepEqual(moveRect(r, 999, 999, B), { x: 50, y: 40, w: 50, h: 40 })
  assert.deepEqual(nudgeRect(r, 'ArrowRight', 1, B), { x: 11, y: 10, w: 50, h: 40 })
  assert.deepEqual(nudgeRect(r, 'ArrowLeft', 10, B), { x: 0, y: 10, w: 50, h: 40 })
  assert.deepEqual(nudgeRect(r, 'KeyQ', 1, B), r)
})

import {
  cssRectToImageRect, mosaicBlocks, arrowHead, pushUndo, toolbarPlacement, cursorFor,
} from '../src/lib/shot.js'

test('CSS 选区换算成整幅图里的物理像素矩形', () => {
  // 本机实测：副屏 slice 起点 x=2560（物理），窗口 CSS 宽 2048，比例 k=1.25
  const slice = { x: 2560, y: 0, w: 2560, h: 1440 }
  assert.deepEqual(
    cssRectToImageRect({ x: 100, y: 200, w: 400, h: 300 }, slice, 2048),
    { x: 2560 + 125, y: 250, w: 500, h: 375 },
  )
  // 宽高至少 1 像素，避免拖出 0 尺寸导致 toBlob 失败
  assert.deepEqual(
    cssRectToImageRect({ x: 0, y: 0, w: 0, h: 0 }, slice, 2048),
    { x: 2560, y: 0, w: 1, h: 1 },
  )
})

test('马赛克块按网格对齐且覆盖整个矩形', () => {
  const blocks = mosaicBlocks({ x: 10, y: 10, w: 20, h: 20 }, 8)
  // x: -6? 不 —— 对齐到 8 的网格：起点 8、16、24，覆盖到 30
  assert.deepEqual(blocks[0], { x: 8, y: 8, w: 8, h: 8 })
  assert.equal(blocks.length, 9) // 3×3 块覆盖 10..30
})

test('箭头两翼对称分布在线段两侧', () => {
  const { p1, p2 } = arrowHead({ x: 0, y: 0 }, { x: 100, y: 0 }, 10)
  assert.deepEqual(p1, { x: 90, y: 5 })
  assert.deepEqual(p2, { x: 90, y: -5 })
  // 零长度线段不产生 NaN
  const z = arrowHead({ x: 5, y: 5 }, { x: 5, y: 5 }, 10)
  assert.ok(Number.isFinite(z.p1.x) && Number.isFinite(z.p1.y))
})

test('撤销栈保留最近 limit 步', () => {
  let s = []
  for (let i = 0; i < 25; i++) s = pushUndo(s, `snap${i}`, 20)
  assert.equal(s.length, 20)
  assert.equal(s[0], 'snap5')
  assert.equal(s[19], 'snap24')
})

test('工具栏优先贴选区下方，越界翻到上方，再越界贴进窗口', () => {
  const win = { x: 0, y: 0, w: 800, h: 600 }
  const bar = { w: 300, h: 40 }
  assert.deepEqual(toolbarPlacement({ x: 100, y: 100, w: 200, h: 150 }, win, bar), {
    x: 100, y: 258, flip: 'below',
  })
  assert.deepEqual(toolbarPlacement({ x: 100, y: 500, w: 200, h: 90 }, win, bar), {
    x: 100, y: 452, flip: 'above',
  })
  // 选区贴满整屏：上下都放不下 → 塞进窗口内
  assert.deepEqual(toolbarPlacement({ x: 0, y: 0, w: 800, h: 600 }, win, bar), {
    x: 0, y: 552, flip: 'inside',
  })
  // 右边界对齐不外溢
  assert.equal(toolbarPlacement({ x: 700, y: 100, w: 100, h: 100 }, win, bar).x, 500)
})

/* ---------------- 遮罩「不闪」的源码级约定（Task 15） ----------------
 *
 * 没有 DOM 夹具（node --test，无 jsdom），所以这里做源码级断言：遮罩窗口是**透明窗**
 * 且建出来即已映射，「不闪」靠的是「底图落地之前页面里没有任何不透明内容」。
 * 这条约定一旦被破坏（压暗层/提示不再挂 painted、或 .shot-root 又写上底色），
 * 用户看到的就是「桌面忽然暗一下 / 白闪」——而构建、既有测试、肉眼 dev 全都不报错。
 */
const overlaySrc = readFileSync(new URL('../src/components/ScreenshotOverlay.vue', import.meta.url), 'utf8')

/* ---------------- 遮罩光标 ----------------
 *
 * 回归背景：光标曾经只按命中区域算（inside → 'move'），不认当前工具。KDE Breeze
 * 主题把 CSS `move` 画成**一只手**（已比对 xcursor-breeze/cursors/move），于是
 * 「框选完 → 选箭头/画笔」之后指针一进选区就变成手，看起来还在拖选区，画不了。
 */
test('除画笔外的标注工具在选区内部是十字，不是移动光标', () => {
  for (const tl of ['rect', 'ellipse', 'arrow', 'mosaic']) {
    assert.equal(cursorFor(tl, 'inside', true), 'crosshair', `${tl} 工具在选区内应显示十字`)
  }
})

test('画笔工具在选区内是笔形光标：自带图片 + 热点 + 十字兜底', () => {
  const c = cursorFor('pen', 'inside', true)
  // 热点必须落在笔尖（3,3）；末尾的 crosshair 是兜底 —— 图片没加载出来也不能变成默认箭头
  assert.match(c, /^url\("data:image\/png;base64,[A-Za-z0-9+/=]+"\) 3 3, crosshair$/,
    '笔形光标必须是「内嵌图片 + 热点 + crosshair 兜底」')
  // 只有画笔画布才给笔形：手柄、选区外、没框选时都不该是笔
  assert.equal(cursorFor('pen', 'nw', true), 'nwse-resize')
  assert.equal(cursorFor('pen', 'outside', true), 'crosshair')
  assert.equal(cursorFor('pen', '', false), 'crosshair')
})

test('笔形光标的图片是 32×32 的合法 PNG（三种内核都要能解）', () => {
  const m = cursorFor('pen', 'inside', true).match(/base64,([A-Za-z0-9+/=]+)"/)
  assert.ok(m, '光标里必须内嵌 base64 图片')
  const buf = Buffer.from(m[1], 'base64')
  assert.equal(buf.subarray(0, 8).toString('hex'), '89504e470d0a1a0a', 'PNG 魔数不对')
  // IHDR 的宽高：8 字节魔数 + 4 字节块长 + 4 字节块类型之后
  assert.equal(buf.readUInt32BE(16), 32, '宽度必须是 32（Windows 自定义光标上限）')
  assert.equal(buf.readUInt32BE(20), 32, '高度必须是 32')
  assert.equal(buf.subarray(12, 16).toString('ascii'), 'IHDR')
})

test('只有移动工具在选区内部是移动光标，文字工具是 I 形光标', () => {
  assert.equal(cursorFor('move', 'inside', true), 'move')
  assert.equal(cursorFor('text', 'inside', true), 'text')
})

test('手柄缩放光标与工具无关；选区外/未框选一律十字', () => {
  const handle = {
    nw: 'nwse-resize', se: 'nwse-resize', ne: 'nesw-resize', sw: 'nesw-resize',
    n: 'ns-resize', s: 'ns-resize', e: 'ew-resize', w: 'ew-resize',
  }
  for (const [h, want] of Object.entries(handle)) {
    for (const tl of ['move', 'arrow', 'text']) {
      assert.equal(cursorFor(tl, h, true), want, `${tl} 工具下手柄 ${h} 仍应是缩放光标`)
    }
  }
  assert.equal(cursorFor('move', 'outside', true), 'crosshair')
  assert.equal(cursorFor('arrow', 'outside', true), 'crosshair')
  assert.equal(cursorFor('move', '', false), 'crosshair')
  // 没有选区时工具再花哨也是十字：这一笔要么新建选区、要么画不了
  assert.equal(cursorFor('arrow', 'inside', false), 'crosshair')
})

test('遮罩组件的光标全部来自 cursorFor，不再自己写一套映射', () => {
  assert.match(overlaySrc, /import \{[^}]*cursorFor[^}]*\} from '\.\.\/lib\/shot'/,
    'ScreenshotOverlay 必须从 lib/shot 引入 cursorFor')
  assert.match(overlaySrc, /const cursor = computed\(\(\) => cursorFor\(/,
    'cursor 必须由 cursorFor 统一决定')
  assert.doesNotMatch(overlaySrc, /inside:\s*'move'/,
    'inside → move 的硬编码会绕开工具判断（手形光标回归）')
})

test('底图落地前不画任何不透明层：压暗层/提示都挂在 painted 上', () => {
  // 两个不透明层必须带 painted 条件
  assert.match(overlaySrc, /v-if="!sel && painted"\s+class="dim-full"/,
    '.dim-full 必须先判断 painted，否则底图没上来就先把整屏压暗')
  assert.match(overlaySrc, /v-if="!sel && !errMsg && painted"\s+class="tip"/,
    '.tip 必须先判断 painted，否则透明窗上会先冒出一块提示')
  // 根节点不能有底色（透明窗上就是个色块）
  assert.match(overlaySrc, /\.shot-root\s*\{[^}]*background:\s*transparent/,
    '.shot-root 背景必须是 transparent')
  // painted 只能在 paintBase() 里、drawImage 之后置真
  const paint = overlaySrc.slice(overlaySrc.indexOf('function paintBase()'))
  const body = paint.slice(0, paint.indexOf('\n}'))
  assert.ok(body.includes('painted.value = true'), 'paintBase() 里必须置 painted')
  assert.ok(body.indexOf('drawImage') < body.indexOf('painted.value = true'),
    'painted 必须在底图 drawImage 之后才置真')
})
