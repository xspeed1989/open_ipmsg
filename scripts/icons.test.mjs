// node --test scripts/ —— 图标绘制契约
//
// 起因：侧边栏那个齿轮是手搓路径 —— 外圈描边 1.3、内孔 1.6（同一个图标两种粗细），
// 齿宽齿高还各不相同，摆在只有线性图标的一栏里就显得歪。构建和既有测试都发现不了
// 这种「画得不好看」，所以这里把两条能机械校验的规矩钉死。
import { readFileSync, readdirSync } from 'node:fs'
import { test } from 'node:test'
import assert from 'node:assert/strict'

const COMPONENTS = new URL('../src/components/', import.meta.url)
const files = readdirSync(COMPONENTS, { encoding: 'utf8' }).filter((f) => f.endsWith('.vue'))

/** 抽出每个 <svg> 的源码片段（含起止行号，报错时能直接定位） */
function svgsOf(source) {
  const out = []
  for (const m of source.matchAll(/<svg\b[\s\S]*?<\/svg>/g)) {
    out.push({ svg: m[0], line: source.slice(0, m.index).split('\n').length })
  }
  return out
}

test('一个图标内描边粗细必须一致（不许 1.3 配 1.6 这种）', () => {
  const bad = []
  for (const f of files) {
    const src = readFileSync(new URL(f, COMPONENTS), 'utf8')
    for (const { svg, line } of svgsOf(src)) {
      const widths = [...new Set([...svg.matchAll(/stroke-width="([\d.]+)"/g)].map((m) => m[1]))]
      if (widths.length > 1) bad.push(`${f}:${line} 混用描边宽度 ${widths.join(' / ')}`)
    }
  }
  assert.deepEqual(bad, [], `同一个图标里混用描边粗细会显得忽粗忽细：\n${bad.join('\n')}`)
})

test('侧边栏齿轮：齿形规整（45° 旋转对称）+ 与内孔同粗细', () => {
  const src = readFileSync(new URL('SideBar.vue', COMPONENTS), 'utf8')
  const svg = svgsOf(src).find((s) => s.svg.includes('circle'))?.svg || ''
  assert.ok(svg, 'SideBar 里应有设置齿轮图标')

  const widths = [...new Set([...svg.matchAll(/stroke-width="([\d.]+)"/g)].map((m) => m[1]))]
  assert.deepEqual(widths, ['1.6'], '齿轮外圈与内孔都用 1.6（全应用图标的主口径）')

  // 齿形：只认「绝对坐标 M/L/Z 多边形」这一种写法，才能机械校验对称性。
  // 换造型（比如改用别的齿轮画法）时请同步改本用例，别把校验绕过去。
  const d = svg.match(/\sd="([^"]+)"/)?.[1] || ''
  assert.match(d, /^M[\d.\sL]+Z$/, '齿轮路径应是绝对坐标多边形（M … L … Z）')
  const pts = d
    .slice(1, -1)
    .split('L')
    .map((p) => p.trim().split(/\s+/).map(Number))
  assert.ok(pts.length >= 24 && pts.length % 8 === 0, `8 齿齿轮应有 8 的倍数个顶点，实际 ${pts.length}`)

  // 8 齿 = 45° 旋转对称：每个顶点转 45° 后都应落在某个顶点上（容差 0.05px，
  // 只留给两位小数的取整误差；手绘歪齿会偏出好几个像素）
  const [cx, cy] = [12, 12]
  const rot45 = ([x, y]) => {
    const a = Math.PI / 4
    return [cx + (x - cx) * Math.cos(a) - (y - cy) * Math.sin(a), cy + (x - cx) * Math.sin(a) + (y - cy) * Math.cos(a)]
  }
  let worst = 0
  for (const p of pts) {
    const r = rot45(p)
    worst = Math.max(worst, Math.min(...pts.map((q) => Math.hypot(r[0] - q[0], r[1] - q[1]))))
  }
  assert.ok(worst < 0.05, `齿形不规整：转 45° 后最大偏差 ${worst.toFixed(2)}px（应 < 0.05）`)
})
