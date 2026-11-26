-- 中英文：T(中文, English) 按 DroidBridge 的界面语言取一个
local EN = db.lang == "en"
return function(zh, en) return EN and en or zh end
