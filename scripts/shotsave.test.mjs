// node --test scripts/ —— 截图落盘纯函数（目录 / 文件名 / 重名避让）
//
// 为什么要单独守：截图「保存」以前弹原生保存框（遮罩窗口里弹原生模态 = 真机上
// 对话框不可见 + 遮罩整层消失），改成固定目录直写。目录从哪来、文件叫什么、
// 撞名怎么排，全在 src/lib/shotSave.js —— 三件事都影响用户磁盘上的实际文件名，
// 不能只靠真机点一次。
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  SHOT_DIR_NAME, shotsDir, joinPath, baseNameOf, shotFileName, freeName, MAX_DUPLICATES,
} from '../src/lib/shotSave.js'

test('目录是图片根目录下的固定子目录名，不自己拼 ~', () => {
  assert.equal(SHOT_DIR_NAME, 'OpenIPMsg 截图')
  assert.equal(shotsDir('/home/allen/Pictures'), '/home/allen/Pictures/OpenIPMsg 截图')
  // 后端给的根目录可能带尾斜杠（不同平台的 dirs 实现不一致），不能拼出双斜杠
  assert.equal(shotsDir('/home/allen/Pictures/'), '/home/allen/Pictures/OpenIPMsg 截图')
  assert.equal(shotsDir('/home/allen/Pictures///'), '/home/allen/Pictures/OpenIPMsg 截图')
  // Windows 盘符路径用反斜杠
  assert.equal(shotsDir('C:\\Users\\a\\Pictures'), 'C:\\Users\\a\\Pictures\\OpenIPMsg 截图')
  // 极端：后端没给出根目录时不造出以分隔符开头的相对路径
  assert.equal(shotsDir(''), SHOT_DIR_NAME)
})

test('目录 + 文件名拼路径，不产生双分隔符', () => {
  assert.equal(joinPath('/a/b', 'x.png'), '/a/b/x.png')
  assert.equal(joinPath('/a/b/', 'x.png'), '/a/b/x.png')
  assert.equal(joinPath('', 'x.png'), 'x.png')
})

test('文件名用本地时间：截图-YYYYMMDD-HHMMSS.png', () => {
  // 构造本地时间（不是 UTC）：晚上 20:05:09 就该写 200509
  const d = new Date(2026, 10, 5, 20, 5, 9) // 2026-11-05 20:05:09 本地
  assert.equal(shotFileName(d), '截图-20261105-200509.png')
  // 个位数月/日/时/分/秒补零
  assert.equal(shotFileName(new Date(2026, 0, 2, 3, 4, 5)), '截图-20260102-030405.png')
  assert.equal(shotFileName(new Date(2026, 11, 31, 23, 59, 59)), '截图-20261231-235959.png')
  // 传时间戳/字符串也要能用（调用方不一定给 Date）
  assert.equal(shotFileName(new Date(2026, 10, 5, 20, 5, 9).getTime()), '截图-20261105-200509.png')
})

test('重名避让：不覆盖，按 -2 / -3 往后排', () => {
  const name = '截图-20261105-200509.png'
  assert.equal(freeName(name, []), name)
  assert.equal(freeName(name, ['别的.png']), name)
  assert.equal(freeName(name, [name]), '截图-20261105-200509-2.png')
  assert.equal(freeName(name, [name, '截图-20261105-200509-2.png']), '截图-20261105-200509-3.png')
  // 空档要能钻：占了 name 与 -3，得排到 -2 而不是 -4
  assert.equal(freeName(name, [name, '截图-20261105-200509-3.png']), '截图-20261105-200509-2.png')
  // 后缀插在扩展名之前，不是接在 .png 后面
  assert.equal(freeName('a.png', ['a.png']), 'a-2.png')
  // 没有扩展名/点开头的文件也不出事
  assert.equal(freeName('a', ['a']), 'a-2')
  assert.equal(freeName('.hidden', ['.hidden']), '.hidden-2')
  // taken 里有非字符串（列目录时可能拿到怪东西）也不炸
  assert.equal(freeName(name, [null, undefined, 3]), name)
})

test('重名避让有上限，不会无限尝试', () => {
  const name = 'a.png'
  const taken = [name]
  for (let i = 2; i <= MAX_DUPLICATES + 1; i++) taken.push(`a-${i}.png`)
  assert.throws(() => freeName(name, taken), /重名过多/)
})

test('提示里只说文件名，不糊整条路径', () => {
  assert.equal(baseNameOf('/home/allen/Pictures/OpenIPMsg 截图/截图-1.png'), '截图-1.png')
  assert.equal(baseNameOf('C:\\a\\b.png'), 'b.png')
  assert.equal(baseNameOf('x.png'), 'x.png')
  assert.equal(baseNameOf(''), '')
})
