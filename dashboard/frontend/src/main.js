import 'virtual:svg-icons-register';

import { use, registerMap } from 'echarts';
import { CanvasRenderer } from 'echarts/renderers';
import { BarChart } from 'echarts/charts';
import {
    GridComponent,
    TooltipComponent,
    VisualMapComponent,
} from 'echarts/components';
use([
    CanvasRenderer,
    BarChart,
    GridComponent,
    TooltipComponent,
    VisualMapComponent,
]);
import { china, global } from './echarts/index.ts';

registerMap('china', china);
registerMap('global', global);
